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
             patch('validation.retained_lane_commands.tree_id',side_effect=['old','changed']):
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
