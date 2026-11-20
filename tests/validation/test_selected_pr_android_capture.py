import copy
import json
import unittest
from unittest.mock import patch
from validation import selected_pr_android_capture as selected
from validation import python_unit_receipts as receipts


class SelectedPrAndroidCaptureTests(unittest.TestCase):
    def test_source_census_is_independent_of_xml_and_requires_final_plain_junit_classes(self):
        raw = b'package tv.plurx.app.player\nclass Fixture {\n @Test fun alpha() {}\n @Test\n fun beta() {}\n}\n'
        expected = {'tv.plurx.app.player.Fixture/alpha', 'tv.plurx.app.player.Fixture/beta'}
        self.assertEqual(selected.source_methods(raw, 'tv.plurx.app.player.Fixture'), expected)
        for bad in (raw.replace(b'class Fixture {', b'class Fixture : Parent() {'),
                    raw.replace(b'class Fixture', b'open class Fixture'),
                    raw.replace(b'@Test fun alpha', b'@Test(expected=Error::class) fun alpha'),
                    raw.replace(b'@Test fun alpha', b'@Ignore @Test fun alpha'),
                    raw.replace(b'fun beta', b'fun alpha')):
            with self.subTest(raw=bad), self.assertRaises(receipts.ReceiptError):
                selected.source_methods(bad, 'tv.plurx.app.player.Fixture')

    def test_release_default_and_body_asserted_capture_cannot_admit_local_rows(self):
        for selected_pr in (False, None):
            with self.assertRaises(receipts.ReceiptError):
                selected.outcomes({}, {'origin': {'kind': 'attested-local'}}, b'', set(), 'a'*40, selected_pr, lambda s:s)
        with self.assertRaises(receipts.ReceiptError):
            selected.outcomes({}, {'origin': {'kind': 'authenticated-ci'}}, b'', set(), 'a'*40, True, lambda s:s)

    def test_unreviewed_or_modified_custody_never_reaches_native_xml(self):
        record = {'origin': {'kind': 'attested-local', 'execution': {
            'mode':'source-cohort-v1', 'kind':'android_jvm', 'selected_pr_local': {
            'version':selected.POLICY, 'custody':{}, 'wrapper':{}, 'daemon':{}}}}}
        with patch('validation.nonrust_unit_cohort.blob', return_value=b'{}'), \
             patch('validation.nonrust_unit_cohort.junit_archive') as xml:
            with self.assertRaises(receipts.ReceiptError):
                selected.outcomes({}, record, b'', set(), 'a'*40, True, lambda s:s)
            xml.assert_not_called()

    def test_recipe_source_and_java_mutations_refuse_before_xml(self):
        base = {'schema':'android-focused-execution-custody-v1', 'source_commit':'a'*40,
                'argv':['./gradlew'], 'exit_code':0, 'wrapper':{'tool_exit_code':0,'sha256':'wrapper'},
                'raw_log_sha256':'log', 'gradle_daemon':{'log_sha256':'daemon'}}
        record = {'source_commit':'a'*40,'exit_code':0,'origin':{'kind':'attested-local','command':['./gradlew'],
            'execution':{'mode':'source-cohort-v1','kind':'android_jvm','selected_pr_local':{
            'version':selected.POLICY,'custody':{},'wrapper':{},'daemon':{}}}}}
        for mutate in (lambda d:d.update(source_commit='b'*40),lambda d:d.update(argv=['./gradlew','--init-script','evil']),
                       lambda d:d.update(raw_log_sha256='forged')):
            custody=copy.deepcopy(base);mutate(custody)
            with patch('validation.nonrust_unit_cohort.blob', return_value=b'{}'), \
                 patch.object(selected.retention,'digest',return_value=selected.CUSTODY_SHA256), \
                 patch.object(selected.r,'bounded_json',return_value=custody), \
                 patch('validation.nonrust_unit_cohort.junit_archive') as xml:
                with self.subTest(custody=custody), self.assertRaises(receipts.ReceiptError):
                    selected.outcomes({},record,b'',set(),'a'*40,True,lambda s:s)
                xml.assert_not_called()

    def capture(self, methods=('alpha', 'beta'), klass='tv.plurx.app.player.Fixture'):
        import base64
        import io
        import tarfile
        proof={'logs':{}}
        def blob(raw):
            digest=selected.retention.digest(raw)
            proof['logs'][digest]={'sha256':digest,'base64':base64.b64encode(raw).decode()}
            return {'sha256_ref':digest}
        xml=('<testsuite tests="'+str(len(methods))+'" failures="0" errors="0">'+''.join(
            '<testcase classname="'+klass+'" name="'+name+'"/>' for name in methods)+'</testsuite>').encode()
        member='TEST-'+klass+'.xml'
        buffer=io.BytesIO()
        with tarfile.open(fileobj=buffer,mode='w:gz') as tar:
            entry=tarfile.TarInfo(member);entry.size=len(xml);tar.addfile(entry,io.BytesIO(xml))
        archive=buffer.getvalue()
        envelope=json.dumps({'sha256':selected.retention.digest(archive),
                             'base64':base64.b64encode(archive).decode()}).encode()
        raw=b'> Task :app:testDebugUnitTest\nBUILD SUCCESSFUL in 1s\n'
        argv=['./gradlew','--no-daemon','--offline','--max-workers=2',':app:testDebugUnitTest','--tests','tv.plurx.app.player.Fixture']
        custody={'schema':'android-focused-execution-custody-v1','source_commit':'a'*40,'argv':argv,
            'exit_code':0,'wrapper':{'tool_exit_code':0,'sha256':selected.retention.digest(b'wrapper')},
            'raw_log_sha256':selected.retention.digest(raw),'gradle_daemon':{'log_sha256':selected.retention.digest(b'daemon'),'observed_java_home':'/java'},
            'execution_source_mode':'direct-clean-checkout','archive_sha256':None,
            'source_clean_start':{'value':True},'source_head_end':{'value':'a'*40},'source_clean_end':{'value':True},
            'post_observations':{'java_binary_sha256':'814dc07211a49b57d8911d6e95fb8c8882ebbd1860dbee910b2f38390c2d4014','java_version':'observed'},
            'java_version_before':'observed','java_home':'/java',
            'junit_xml':{member:{'size':len(xml),'sha256':selected.retention.digest(xml)}}}
        custody_raw=json.dumps(custody).encode()
        record={'source_commit':'a'*40,'exit_code':0,'origin':{'kind':'attested-local','command':argv,
            'execution':{'mode':'source-cohort-v1','kind':'android_jvm','selected_pr_local':{
                'version':selected.POLICY,'custody':blob(custody_raw),'wrapper':blob(b'wrapper'),'daemon':blob(b'daemon')}}},
            'coverage':{'mode':'junit-xml','xml_archive':blob(envelope)}}
        source=b'package tv.plurx.app.player\nclass Fixture {\n @Test fun alpha() {}\n @Test fun beta() {}\n}\n'
        return proof,record,raw,source,selected.retention.digest(custody_raw)

    def test_complete_source_derived_class_census_accepts_original_native_rows(self):
        proof,record,raw,source,digest=self.capture()
        universe={'tv.plurx.app.player.Fixture/alpha','tv.plurx.app.player.Fixture/beta'}
        with patch.object(selected,'CUSTODY_SHA256',digest), \
             patch.object(selected.retention,'git_bytes',return_value=source), \
             patch('validation.retained_lane_commands.tree_id',return_value='tree'):
            self.assertEqual(selected.outcomes(proof,record,raw,universe,'b'*40,True,lambda s:s),
                             {key:'pass' for key in universe})

    def test_partial_missing_and_foreign_class_xml_never_qualifies_a_complete_selector(self):
        universe={'tv.plurx.app.player.Fixture/alpha','tv.plurx.app.player.Fixture/beta','tv.plurx.app.player.Other/alpha'}
        for methods,klass in [(('alpha',),'tv.plurx.app.player.Fixture'),((),'tv.plurx.app.player.Fixture'),(('alpha',),'tv.plurx.app.player.Other')]:
            proof,record,raw,source,digest=self.capture(methods,klass)
            with patch.object(selected,'CUSTODY_SHA256',digest), \
                 patch.object(selected.retention,'git_bytes',return_value=source), \
                 patch('validation.retained_lane_commands.tree_id',return_value='tree'):
                with self.subTest(methods=methods,klass=klass), self.assertRaises(receipts.ReceiptError):
                    selected.outcomes(proof,record,raw,universe,'b'*40,True,lambda s:s)

    def test_changed_android_tree_and_changed_binary_are_refused(self):
        proof,record,raw,source,digest=self.capture()
        universe={'tv.plurx.app.player.Fixture/alpha','tv.plurx.app.player.Fixture/beta'}
        with patch.object(selected,'CUSTODY_SHA256',digest), \
             patch.object(selected,'android_sources_compatible',return_value=False):
            with self.assertRaises(receipts.ReceiptError):
                selected.outcomes(proof,record,raw,universe,'b'*40,True,lambda s:s)
        custody_ref=record['origin']['execution']['selected_pr_local']['custody']['sha256_ref']
        import base64
        custody=json.loads(base64.b64decode(proof['logs'][custody_ref]['base64']))
        custody['post_observations']['java_binary_sha256']='0'*64
        altered=json.dumps(custody).encode();changed_digest=selected.retention.digest(altered)
        proof['logs'][changed_digest]={'sha256':changed_digest,'base64':base64.b64encode(altered).decode()}
        record['origin']['execution']['selected_pr_local']['custody']={'sha256_ref':changed_digest}
        with patch.object(selected,'CUSTODY_SHA256',changed_digest), \
             patch('validation.retained_lane_commands.tree_id',return_value='tree'):
            with self.assertRaises(receipts.ReceiptError):
                selected.outcomes(proof,record,raw,universe,'b'*40,True,lambda s:s)

    def measured_capture(self):
        import base64
        proof,record,raw,source,_=self.capture()
        universe={'tv.plurx.app.player.Fixture/alpha','tv.plurx.app.player.Fixture/beta'}
        ref=record['coverage']['xml_archive']['sha256_ref']
        envelope=json.loads(base64.b64decode(proof['logs'][ref]['base64']))
        argv=['./gradlew','--no-daemon','--offline','--max-workers=2',':app:testDebugUnitTest']
        for ident in sorted(universe):argv+=['--tests',ident.replace('/','.')]
        binding={'schema':selected.MEASURED_POLICY,'source_commit':'a'*40,'source_archive_sha256':'archive',
            'argv':argv,'source_head_start':'a'*40,'source_head_end':'a'*40,'source_clean_start':True,'source_clean_end':True,
            'started_utc':'2026-11-20T20:00:00Z','finished_utc':'2026-11-20T20:00:01Z','exit_code':0,
            'log_sha256':selected.retention.digest(raw),'xml_archive_sha256':envelope['sha256'],
            'java_version_before':'openjdk version "25.0.4" observed','java_version_after':'openjdk version "25.0.4" observed',
            'java_binary_sha256_before':'814dc07211a49b57d8911d6e95fb8c8882ebbd1860dbee910b2f38390c2d4014',
            'java_binary_sha256_after':'814dc07211a49b57d8911d6e95fb8c8882ebbd1860dbee910b2f38390c2d4014',
            'platform':'darwin-arm64','gradle_semantic_environment':{},'gradle_init_scripts':[],
            'ids_sha256':selected.retention.digest(json.dumps(sorted(universe),separators=(',',':')).encode())}
        record['origin']['command']=argv
        record['origin']['execution']['selected_pr_local']={'version':selected.MEASURED_POLICY}
        return proof,record,raw,universe,binding

    def measured_result(self, proof,record,raw,universe,binding,selected_pr=True):
        import base64
        content=json.dumps(binding).encode();digest=selected.retention.digest(content)
        proof['logs'][digest]={'sha256':digest,'base64':base64.b64encode(content).decode()}
        record['origin']['execution']['selected_pr_local']['binding']={'sha256_ref':digest}
        with patch('validation.qualification_unit_continuation.source_archive_digest',return_value='archive'), \
             patch('validation.retained_lane_commands.tree_id',return_value='tree'):
            return selected.measured_outcomes(proof,record,raw,universe,'b'*40,selected_pr,lambda s:s)

    def test_measured_exact_method_capture_admits_only_its_native_current_rows(self):
        proof,record,raw,universe,binding=self.measured_capture()
        self.assertEqual(self.measured_result(proof,record,raw,universe,binding),{key:'pass' for key in universe})

    def test_measured_capture_refuses_class_prefix_duplicate_and_missing_method_outcomes(self):
        for selectors in (['tv.plurx.app.player.Fixture'],['tv.plurx.app.player.Fixture.alpha']*2,
                          ['tv.plurx.app.player.Fixture.alpha']):
            proof,record,raw,universe,binding=self.measured_capture()
            argv=binding['argv'][:5]
            for name in selectors:argv+=['--tests',name]
            binding['argv']=argv;record['origin']['command']=argv
            with self.subTest(selectors=selectors),self.assertRaises(receipts.ReceiptError):
                self.measured_result(proof,record,raw,universe,binding)

    def test_measured_capture_requires_pre_and_post_source_binary_inventory_and_recipe_bindings(self):
        mutations={'java_binary_sha256_after':'0'*64,'source_head_end':'b'*40,'source_clean_end':False,
                   'source_archive_sha256':'forged','ids_sha256':'forged','gradle_init_scripts':['evil'],
                   'gradle_semantic_environment':{'JAVA_TOOL_OPTIONS':'-javaagent:evil'},'platform':'linux-x64'}
        for field,value in mutations.items():
            proof,record,raw,universe,binding=self.measured_capture();binding[field]=value
            with self.subTest(field=field),self.assertRaises(receipts.ReceiptError):
                self.measured_result(proof,record,raw,universe,binding)
        proof,record,raw,universe,binding=self.measured_capture()
        with self.assertRaises(receipts.ReceiptError):
            self.measured_result(proof,record,raw,universe,binding,False)

    def test_current_build_recipe_pins_apply_only_to_selected_gate_and_refuse_changed_inputs(self):
        from validation import nonrust_unit_cohort as cohort
        current=selected.retention.digest(b'current')
        legacy=selected.retention.digest(b'legacy')
        with patch.object(selected,'BUILD_INPUTS',{'config':current}), \
             patch.object(cohort,'CURRENT_JVM_BUILD_INPUTS',{'config':legacy}), \
             patch.object(cohort.retention,'git_bytes',return_value=b'current'):
            cohort.require_jvm_build_inputs('a'*40,True)
            with self.assertRaises(receipts.ReceiptError):
                cohort.require_jvm_build_inputs('a'*40)
        with patch.object(selected,'BUILD_INPUTS',{'config':current}), \
             patch.object(cohort.retention,'git_bytes',return_value=b'changed toolchain'):
            with self.assertRaises(receipts.ReceiptError):
                cohort.require_jvm_build_inputs('a'*40,True)

    def counter_sources(self, mutation=None):
        build='clients/android/app/build.gradle.kts';readme='clients/android/README.md'
        before={build:b'android {\n    versionCode = 209\n    versionName = "0.8.0"\n}\n',
                readme:b'> Status: **v0.8.0**, build `209` - same behavior.\n'}
        after={path:raw.replace(b'209',b'210') for path,raw in before.items()}
        if mutation:mutation(after)
        return before,after

    def counter_compatible(self,before,after,paths=None,mode='100644'):
        build='clients/android/app/build.gradle.kts'
        with patch('validation.retained_lane_commands.tree_id',side_effect=lambda revision,path:revision), \
             patch.object(selected.retention,'changed',return_value=set(before) if paths is None else paths), \
             patch.object(selected.retention,'git_bytes',side_effect=lambda revision,path:(before if revision=='a'*40 else after)[path]), \
             patch.object(selected,'_android_file_mode',return_value=mode), \
             patch.object(selected,'ANDROID_209_BUILD_SHA256',selected.retention.digest(before[build])), \
             patch.object(selected,'BUILD_INPUTS',{build:selected.retention.digest(after[build])}):
            return selected.android_sources_compatible('a'*40,'b'*40)

    def test_counter_projection_preserves_actual_209_provenance_only_for_the_two_210_literals(self):
        before,after=self.counter_sources()
        self.assertTrue(self.counter_compatible(before,after))
        self.assertIn(b'209',before['clients/android/app/build.gradle.kts'])
        self.assertNotEqual(before,after)

    def test_counter_projection_refuses_build_flags_source_or_unrelated_readme_changes(self):
        build='clients/android/app/build.gradle.kts';readme='clients/android/README.md'
        for mutation in (lambda rows:rows.update({build:rows[build]+b'tasks.test { enabled=false }\n'}),
                         lambda rows:rows.update({readme:rows[readme]+b'Other behavior changed.\n'}),
                         lambda rows:rows.update({build:rows[build].replace(b'210',b'211')})):
            before,after=self.counter_sources(mutation)
            with self.assertRaises(receipts.ReceiptError):
                self.counter_compatible(before,after)
        before,after=self.counter_sources()
        with self.assertRaises(receipts.ReceiptError):
            self.counter_compatible(before,after,paths=set(before)|{'clients/android/app/src/main/Changed.kt'})

    def test_counter_projection_refuses_type_modes_missing_or_duplicate_counter_fields(self):
        before,after=self.counter_sources()
        with self.assertRaises(receipts.ReceiptError):
            self.counter_compatible(before,after,mode='100755')
        for extra in (b'\n    versionCode = 210\n',b''):
            before,after=self.counter_sources()
            path='clients/android/app/build.gradle.kts'
            after[path]=after[path]+extra if extra else after[path].replace(b'    versionCode = 210\n',b'')
            with self.assertRaises(receipts.ReceiptError):
                self.counter_compatible(before,after)
