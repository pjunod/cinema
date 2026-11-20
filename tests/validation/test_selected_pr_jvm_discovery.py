"""Closed selected-PR runner metadata; ordinary inventory stays strict."""
import copy
import unittest
from unittest.mock import patch
from validation import jvm_unit_discovery as discovery
from tests.validation.test_jvm_unit_discovery import FIXTURE


class SelectedPrJvmDiscoveryCase(unittest.TestCase):
    def metadata(self):
        return discovery.native_class_details(FIXTURE)

    def reviewed_runner_fixture(self):
        import hashlib
        header=discovery.class_header(FIXTURE)
        header['name']=discovery.ROBOLECTRIC_CLASS
        metadata=self.metadata()
        metadata['source_file']='SharingInvitationFileProviderTest.kt'
        metadata['annotations'].append(('RuntimeVisibleAnnotations','Lorg/junit/runner/RunWith;'))
        method=next(m for m in metadata['methods'] if m['name']=='neverExecuteThisBody')
        metadata['methods']=[m for m in metadata['methods'] if m['name']!='neverExecuteThisBody']+[
            {**copy.deepcopy(method),'name':name.rsplit('.',1)[-1]} for name in sorted(discovery.ROBOLECTRIC_METHODS)]
        return header,metadata,hashlib.sha256(FIXTURE).hexdigest()

    def test_only_the_sealed_selected_pr_runner_has_exact_two_required_methods(self):
        header,metadata,digest=self.reviewed_runner_fixture()
        roster={discovery.ROBOLECTRIC_CLASS+'.class':FIXTURE}
        with patch.object(discovery,'class_header',return_value=header), \
             patch.object(discovery,'native_class_details',return_value=metadata), \
             patch.object(discovery,'ROBOLECTRIC_CLASS_SHA256',digest):
            with self.assertRaises(discovery.InventoryError):
                discovery.native_junit4_inventory(roster)
            result=discovery.native_junit4_inventory(roster,selected_pr_contract=discovery.SELECTED_PR_DISCOVERY)
            self.assertEqual(set(result['ids']),discovery.ROBOLECTRIC_METHODS)
            self.assertIsNone(result['unit_runtime_outcomes'])

    def test_selected_runner_cannot_hide_changed_bytes_foreign_runner_skip_rule_or_partial_census(self):
        header,metadata,digest=self.reviewed_runner_fixture()
        for mutation in ('bytes','foreign','skip','rule','partial'):
            h=copy.deepcopy(header);d=copy.deepcopy(metadata);pin=digest
            if mutation=='bytes':pin='0'*64
            elif mutation=='foreign':h['name']='fixture/UnreviewedRunner'
            elif mutation=='skip':d['annotations'].append(('RuntimeVisibleAnnotations','Lorg/junit/Ignore;'))
            elif mutation=='rule':d['fields'].append({'annotations':[('RuntimeVisibleAnnotations','Lorg/junit/Rule;')]})
            else:d['methods']=d['methods'][:-1]
            with patch.object(discovery,'class_header',return_value=h), \
                 patch.object(discovery,'native_class_details',return_value=d), \
                 patch.object(discovery,'ROBOLECTRIC_CLASS_SHA256',pin):
                with self.subTest(mutation=mutation),self.assertRaises(discovery.InventoryError):
                    discovery.native_junit4_inventory({h['name']+'.class':FIXTURE},selected_pr_contract=discovery.SELECTED_PR_DISCOVERY)

    def test_selected_runner_binds_source_sdk_config_version_and_actual_jar_bytes(self):
        import hashlib
        from pathlib import Path
        import tempfile
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/discovery.ROBOLECTRIC_SOURCE
            source.parent.mkdir(parents=True);source.write_bytes(b'sealed SDK35 source')
            catalog=root/'clients/android/gradle/libs.versions.toml';catalog.parent.mkdir(parents=True)
            catalog.write_text('[versions]\nrobolectric="4.17"\n')
            jar=root/'robolectric-4.17.jar';jar.write_bytes(b'sealed runner jar')
            with patch.object(discovery,'ROBOLECTRIC_SOURCE_SHA256',hashlib.sha256(source.read_bytes()).hexdigest()), \
                 patch.object(discovery,'ROBOLECTRIC_JAR_SHA256',hashlib.sha256(jar.read_bytes()).hexdigest()):
                discovery.selected_pr_discovery_inputs(root,jar)
                for kind in ('source','version','jar'):
                    before=(source.read_bytes(),catalog.read_text(),jar.read_bytes())
                    if kind=='source':source.write_bytes(b'changed SDK or runner')
                    elif kind=='version':catalog.write_text('[versions]\nrobolectric="4.18"\n')
                    else:jar.write_bytes(b'different bytecode same version')
                    with self.subTest(kind=kind),self.assertRaises(discovery.InventoryError):
                        discovery.selected_pr_discovery_inputs(root,jar)
                    source.write_bytes(before[0]);catalog.write_text(before[1]);jar.write_bytes(before[2])
