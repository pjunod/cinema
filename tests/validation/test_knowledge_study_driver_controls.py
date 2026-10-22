"""Synthetic operator boundary controls; no real cases, Gold or providers."""
from pathlib import Path
from types import SimpleNamespace as NS
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

from validation.knowledge.study_driver import document_scoring as docs
from validation.knowledge.study_driver import request_journal as journal
from validation.knowledge.study_driver import source_binding as sources
from validation.knowledge.study_driver import operator_workflow as workflow


def scratch():
    root=Path(__file__).resolve().parents[2]/"target/agent-fixtures/.hermes/hdr-collaboration/workspaces/study-controls"
    root.mkdir(parents=True,exist_ok=True)
    return tempfile.TemporaryDirectory(dir=root)


def reference():
    return {"expected_references":[{"reference_id":"role","role":"synthetic policy",
        "required_coverage":[{"coverage_id":"first","document_proposition":"Use isolated source."},{"coverage_id":"second","document_proposition":"Count actual assertions."}],
        "alternatives":[{"path":"docs/policy.md","canonical_heading":"Runtime policy","supports_coverage_ids":["first","second"],
            "source_sections":[{"line_range":[1,3],"visible_excerpt":"# Runtime policy\nUse isolated source.\nCount actual assertions.\n".rstrip("\n")}]}]}]}


def view():
    text="# Runtime policy\nUse isolated source.\nCount actual assertions.\n\n# Alternate policy\nUse isolated source and count actual assertions.\n"
    return {"files":{"docs/policy.md":{"content":text}},"visibility":{"docs/policy.md":{"content_sha256":hashlib.sha256(text.encode()).hexdigest(),"line_ranges":[[1,6]]}}}


class CitationBoundaryTests(unittest.TestCase):
    def test_partial_union_cannot_claim_full_role(self):
        ref=reference();run=NS(answer="docs/policy.md Runtime policy: use isolated source.",edits=(),spec=NS(id="synthetic"))
        projected=docs.project(ref,view(),run.answer,())
        coverage=[{"id":"first","applied":True,"cited_path":"docs/policy.md","cited_locator":"Runtime policy","source_quote":"Use isolated source.","evidence_quote":"use isolated source.","application_reason":"Isolate this proposal's execution."},
            {"id":"second","applied":False,"cited_path":"","cited_locator":"","source_quote":"","evidence_quote":"","application_reason":""}]
        data={"references":[{"id":"role","found":True,"coverage":coverage}],"unsupported_claims":0,"unsupported_evidence":[]}
        with self.assertRaisesRegex(ValueError,"partial document coverage"):
            docs.parse(data,ref,projected,run,NS(DocumentScore=lambda *a:a),"fresh")

    def test_equivalent_visible_heading_is_projected_without_hidden_lines(self):
        ref=reference();visible=view();visible["visibility"]["docs/policy.md"]["line_ranges"]=[[1,4],[5,6]]
        projected=docs.project(ref,visible,"Apply docs/policy.md § Alternate policy to isolate source and count assertions.",())
        self.assertTrue(any("Use isolated source and count actual assertions." in row["content"] for row in projected["visible_evidence"]))
        for row in projected["visible_evidence"]:
            start,end=row["line_range"]
            self.assertTrue(any(a<=start<=end<=b for a,b in visible["visibility"][row["path"]]["line_ranges"]))

    def test_projection_refuses_changed_sealed_source_bytes(self):
        visible=view();visible["files"]["docs/policy.md"]["content"]+="hidden addition"
        with self.assertRaisesRegex(ValueError,"digest mismatch"):
            docs.project(reference(),visible,"",())


class DurableJournalTests(unittest.TestCase):
    def test_atomic_events_retain_reservation_and_poison_on_write_failure(self):
        with scratch() as directory:
            root=Path(directory);root.chmod(0o700)
            value=journal.PrivateRequestJournal(root/"events","sha256:"+"a"*64)
            value.append({"event":"request-reserved","reservation":17})
            with patch.object(journal.os,"fsync",side_effect=OSError("synthetic disk failure")):
                with self.assertRaises(OSError):value.append({"event":"response-received"})
            self.assertTrue(value.failed)
            with self.assertRaises(ValueError):value.append({"event":"retry"})
            rows=sorted((root/"events").glob("*.private.json"))
            self.assertEqual(len(rows),2)
            self.assertEqual(json.loads(rows[-1].read_bytes())["payload"]["reservation"],17)
            self.assertTrue(all(p.stat().st_mode&0o777==0o600 for p in rows))
            self.assertFalse(any(p.name.startswith(".pending-") for p in (root/"events").iterdir()))

    def test_existing_journal_and_symlink_checkpoint_cannot_be_reused(self):
        with scratch() as directory:
            root=Path(directory);root.chmod(0o700)
            journal.PrivateRequestJournal(root/"events","sha256:"+"a"*64)
            with self.assertRaises(FileExistsError):journal.PrivateRequestJournal(root/"events","sha256:"+"a"*64)
            outside=root/"outside";outside.write_bytes(b"sentinel")
            (root/"checkpoint").symlink_to(outside)
            with self.assertRaises(ValueError):journal.atomic_private(root,"checkpoint",{"changed":True})
            self.assertEqual(outside.read_bytes(),b"sentinel")


class LocalAdmissionTests(unittest.TestCase):
    def test_actual_flock_excludes_other_operator_and_nested_request(self):
        with scratch() as directory:
            root=Path(directory);root.chmod(0o700)
            kwargs={"closure":NS(verify=lambda:True),"review_gate":NS(verify=lambda *a:None),"config":{"reviewed_source_hashes":{},"review_receipt":"synthetic"}}
            lease=workflow.StudyAdmission(root/"lease",{"model":"synthetic"},**kwargs)
            try:
                with self.assertRaises(BlockingIOError):workflow.StudyAdmission(root/"lease",{"model":"synthetic"},**kwargs)
                with lease.request():
                    with self.assertRaisesRegex(ValueError,"parallel operator"):
                        with lease.request():pass
                replacement=root/"replacement";replacement.write_bytes(lease.metadata);replacement.chmod(0o600);os.replace(replacement,lease.path)
                with self.assertRaisesRegex(ValueError,"inode changed"):lease.assert_current()
            finally:lease.close()


class ClosedSourceTests(unittest.TestCase):
    def test_stale_project_modules_are_refused(self):
        with scratch() as directory:
            root=Path(directory);root.chmod(0o700)
            package=root/"package.py";package.write_bytes(b"")
            manifest={"schema_version":1,"modules":{"_study_operator":{"path":str(package),"sha256":hashlib.sha256(b"").hexdigest(),"package":True}},"assets":{}}
            sha=journal.atomic_private(root,"sources.json",manifest)
            closure=sources.SourceClosure(root/"sources.json",sha)
            with self.assertRaisesRegex(ValueError,"stale project"):
                with closure:pass

    def test_exact_buffers_ignore_pyc_and_deny_unapproved_imports(self):
        with scratch() as directory:
            root=Path(directory);root.chmod(0o700)
            approved=b'MARKER="approved-buffer"\n'
            package=root/"__init__.py";package.write_bytes(b"")
            module=root/"example.py";module.write_bytes(approved)
            poison=root/"unreviewed.py";poison.write_text('raise RuntimeError("must never execute")\n')
            manifest={"schema_version":1,"modules":{"_study_operator":{"path":str(package),"sha256":hashlib.sha256(b"").hexdigest(),"package":True},"_study_operator.example":{"path":str(module),"sha256":hashlib.sha256(approved).hexdigest(),"package":False}},"assets":{}}
            sha=journal.atomic_private(root,"sources.json",manifest)
            # Isolated fresh Python avoids importing an already-live validation
            # package. A valid conflicting pyc is present, but never opened.
            program='''import hashlib,importlib.util,marshal,pathlib,struct,sys,types
source=pathlib.Path(sys.argv[1]);raw=source.read_bytes()
assert hashlib.sha256(raw).hexdigest()==sys.argv[2]
loader=types.ModuleType('_exact_bootstrap');loader.__file__=str(source)
sys.modules[loader.__name__]=loader
exec(compile(raw,str(source),'exec',dont_inherit=True),loader.__dict__)
module=pathlib.Path(sys.argv[3]);approved=module.read_bytes()
cache=pathlib.Path(importlib.util.cache_from_source(str(module)));cache.parent.mkdir()
cache.write_bytes(importlib.util.MAGIC_NUMBER+struct.pack('<I',1)+importlib.util.source_hash(approved)+marshal.dumps(compile('MARKER="wrong-cache"',str(module),'exec')))
with loader.SourceClosure(sys.argv[4],sys.argv[5]) as closure:
 import _study_operator.example as example
 assert example.MARKER=='approved-buffer' and example.__cached__ is None
 try:import _study_operator.unreviewed
 except ImportError:pass
 else:raise AssertionError('ambient source accepted')
print('synthetic exact buffer pass')
'''
            result=subprocess.run([sys.executable,"-I","-S","-c",program,sources.__file__,hashlib.sha256(Path(sources.__file__).read_bytes()).hexdigest(),str(module),str(root/"sources.json"),sha],capture_output=True,timeout=30)
            self.assertEqual(result.returncode,0,result.stderr.decode())
            self.assertEqual(result.stdout,b"synthetic exact buffer pass\n")
