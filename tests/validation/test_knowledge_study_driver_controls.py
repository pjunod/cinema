"""Synthetic operator boundary controls; no real cases, Gold or providers."""
from contextlib import nullcontext
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
from validation.knowledge.study_driver import trusted_launcher as launcher
from validation.knowledge import evaluation as ev


def scratch():
    root=Path(__file__).resolve().parents[2]/"target/agent-fixtures/.hermes/hdr-collaboration/workspaces/study-controls"
    root.mkdir(parents=True,exist_ok=True)
    return tempfile.TemporaryDirectory(dir=root)


def reference():
    return {"expected_references":[{"reference_id":"role","role":"synthetic policy",
        "required_coverage":[{"coverage_id":"first","document_proposition":"Use isolated source."},{"coverage_id":"second","document_proposition":"Count actual assertions."}],
        "alternatives":[{"path":"fixtures/policy.md","canonical_heading":"Runtime policy","supports_coverage_ids":["first","second"],
            "source_sections":[{"line_range":[1,3],"visible_excerpt":"# Runtime policy\nUse isolated source.\nCount actual assertions.\n".rstrip("\n")}]}]}]}


def view():
    text="# Runtime policy\nUse isolated source.\nCount actual assertions.\n\n# Alternate policy\nUse isolated source and count actual assertions.\n"
    return {"files":{"fixtures/policy.md":{"content":text}},"visibility":{"fixtures/policy.md":{"content_sha256":hashlib.sha256(text.encode()).hexdigest(),"line_ranges":[[1,6]]}}}


class CitationBoundaryTests(unittest.TestCase):
    def test_partial_union_cannot_claim_full_role(self):
        ref=reference();run=NS(answer="fixtures/policy.md Runtime policy: use isolated source.",edits=(),spec=NS(id="synthetic"))
        projected=docs.project(ref,view(),run.answer,())
        coverage=[{"id":"first","applied":True,"cited_path":"fixtures/policy.md","cited_locator":"Runtime policy","source_quote":"Use isolated source.","evidence_quote":"use isolated source.","application_reason":"Isolate this proposal's execution."},
            {"id":"second","applied":False,"cited_path":"","cited_locator":"","source_quote":"","evidence_quote":"","application_reason":""}]
        data={"references":[{"id":"role","found":True,"coverage":coverage}],"unsupported_claims":0,"unsupported_evidence":[]}
        with self.assertRaisesRegex(ValueError,"partial document coverage"):
            docs.parse(data,ref,projected,run,NS(DocumentScore=lambda *a:a),"fresh")

    def test_equivalent_visible_heading_is_projected_without_hidden_lines(self):
        ref=reference();visible=view();visible["visibility"]["fixtures/policy.md"]["line_ranges"]=[[1,3],[5,6]]
        projected=docs.project(ref,visible,"Apply fixtures/policy.md § Alternate policy to isolate source and count assertions.",())
        self.assertTrue(any("Use isolated source and count actual assertions." in row["content"] for row in projected["visible_evidence"]))
        for row in projected["visible_evidence"]:
            start,end=row["line_range"]
            self.assertTrue(any(a<=start<=end<=b for a,b in visible["visibility"][row["path"]]["line_ranges"]))

    def test_projection_refuses_changed_sealed_source_bytes(self):
        visible=view();visible["files"]["fixtures/policy.md"]["content"]+="hidden addition"
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


class NavigationQualificationTests(unittest.TestCase):
    def fixture(self):
        assets=NS(digest="assets",expected_binary_sha256="binary")
        session=NS(root=Path("/synthetic-visible"),audit={"assets_sha256":"assets","view_sha256":"view","synthetic_head":"head","queries":[{"mode":"outline","receipt":{"binary_sha256":"binary","head":"head","root":"/synthetic-visible"}}]})
        return session,assets

    def test_registry_declares_same_file_symbol_selector_for_every_symbol_mode(self):
        tool=workflow.tool_registry()["ripwire"]
        self.assertIn("outline/body/callers/impact file:symbol",tool["description"])
        self.assertEqual(tool["input_schema"]["properties"]["mode"]["enum"],["find","outline","body","callers","impact"])

    def test_unavailable_dict_and_missing_or_mismatched_provenance_refuse(self):
        session,assets=self.fixture()
        kwargs=dict(assets=assets,expected_view_sha256="view",previous_queries=0)
        with self.assertRaisesRegex(ValueError,"unavailable"):
            workflow.verify_navigation_result(session,{"status":"unavailable","mode":"outline"},"outline",**kwargs)
        session.audit["queries"][-1]["receipt"]["head"]="other"
        with self.assertRaisesRegex(ValueError,"receipt mismatch"):
            workflow.verify_navigation_result(session,{"status":"ok","mode":"outline"},"outline",**kwargs)
        session.audit["queries"]=[]
        with self.assertRaisesRegex(ValueError,"receipt required"):
            workflow.verify_navigation_result(session,{"status":"ok","mode":"outline"},"outline",**kwargs)

    def test_successful_empty_result_needs_exact_verified_fresh_receipt(self):
        session,assets=self.fixture()
        observed=workflow.verify_navigation_result(session,{"status":"ok","mode":"outline","text":"","coverage_warning":"empty heuristic graph is not proof of absence"},"outline",assets=assets,expected_view_sha256="view",previous_queries=0)
        self.assertEqual(observed["status"],"ok")
        self.assertEqual(len(observed["receipt_sha256"]),64)


class CalibrationAdmissionTests(unittest.TestCase):
    def test_measured_envelope_reserves_every_share_before_answers(self):
        values={str(i):1000+i for i in range(12)}
        solver,run,scorer,output,caps,extra=workflow.calibrated_allocation(values,{a:1200 for a in "ABC"},1600,1011,50000)
        self.assertLessEqual(108*run,solver)
        self.assertLessEqual(18*sum(caps.values()),scorer)
        self.assertTrue(all(caps[key]>=count+589+output for key,count in values.items()))
        self.assertLessEqual(solver+scorer+50000,3000000)

    def test_infeasible_measured_stress_or_initial_solver_refuses(self):
        values={str(i):1000 for i in range(12)}
        with self.assertRaisesRegex(ValueError,"216 reserved shares"):
            workflow.calibrated_allocation(values,{a:1200 for a in "ABC"},100000,1000,50000)
        with self.assertRaisesRegex(ValueError,"initial solver envelope"):
            workflow.calibrated_allocation(values,{a:14000 for a in "ABC"},1500,1000,50000)


class ScorerAdmissionTests(unittest.TestCase):
    def fixture(self):
        limits=NS(total_tokens=1000,run_tokens=100,output_tokens=30,request_timeout_seconds=10)
        budget=ev.Budget(limits);events=[]
        backend=NS(respond=Mock(return_value=ev.Response("synthetic score",(),ev.Usage(20,5),"synthetic-request",None,"synthetic","end_turn")))
        entry={"run_id":"synthetic-run","scorer_id":"S1","anonymous_id":"one","reservation":None,"status":"prepared"}
        kwargs=dict(messages=(ev.Message("user","synthetic inert evidence"),),admission=NS(request=nullcontext),scorer_limits=limits,case_cap=100,score_budget=budget,backend=backend,request_journal=NS(append=events.append),batch_tokens=10,dry=10,calibrated=3000000)
        return entry,kwargs,events,backend,budget

    def test_local_oversize_is_durable_unscored_then_later_context_runs(self):
        entry,kwargs,events,backend,budget=self.fixture()
        self.assertIsNone(launcher.perform_scorer_request(entry,count_score=lambda *a:80,**kwargs))
        self.assertEqual(entry["status"],"unscored")
        self.assertEqual(events[-1]["event"],"scorer_share_exhausted")
        self.assertEqual(budget.charged_tokens,0);backend.respond.assert_not_called()
        later={**entry,"anonymous_id":"two","status":"prepared"}
        self.assertIsNotNone(launcher.perform_scorer_request(later,count_score=lambda *a:20,**kwargs))
        self.assertEqual(budget.charged_tokens,25);self.assertEqual(backend.respond.call_count,1)

    def test_remote_or_count_checkpoint_failure_raises_before_dispatch(self):
        for fail in ("remote","count-journal"):
            with self.subTest(fail=fail):
                entry,kwargs,events,backend,budget=self.fixture()
                def counter(*args):
                    if fail=="remote":raise OSError("synthetic quota failure")
                    return 20
                def append(event):
                    if fail=="count-journal" and event["event"]=="scorer_count_received":raise OSError("synthetic journal failure")
                    events.append(event)
                kwargs["request_journal"]=NS(append=append)
                with self.assertRaises(OSError):launcher.perform_scorer_request(entry,count_score=counter,**kwargs)
                backend.respond.assert_not_called();self.assertEqual(budget.charged_tokens,0)

    def test_response_checkpoint_failure_retains_full_reservation(self):
        entry,kwargs,events,backend,budget=self.fixture()
        def append(event):
            if event["event"]=="scorer_response_received":raise OSError("synthetic journal failure")
            events.append(event)
        kwargs["request_journal"]=NS(append=append)
        with self.assertRaises(OSError):launcher.perform_scorer_request(entry,count_score=lambda *a:20,**kwargs)
        self.assertEqual(budget.charged_tokens,50)
        self.assertEqual(entry["reservation"],50)
        self.assertEqual(backend.respond.call_count,1)

    def test_unknown_remote_usage_is_terminal_with_reservation_retained(self):
        entry,kwargs,events,backend,budget=self.fixture()
        backend.respond.return_value=ev.Response("synthetic",(),ev.Usage(),"request",None,"synthetic","end_turn")
        with self.assertRaisesRegex(launcher.LaunchRefusal,"Unaccounted"):
            launcher.perform_scorer_request(entry,count_score=lambda *a:20,**kwargs)
        self.assertTrue(budget.uncertain);self.assertEqual(budget.charged_tokens,50)
