"""Synthetic compatibility controls; no real cases, Gold or provider access."""
import json,os
from types import SimpleNamespace as NS
import unittest
from unittest.mock import Mock,patch
from validation.knowledge.study_driver import trusted_launcher as launcher
from validation.knowledge.study_driver import runtime_bindings as bindings
from validation.knowledge.study_driver import isolation_adapter as adapter
from validation.knowledge.study_driver import python_byte_outcome_driver as byte_driver

class ReviewContractTests(unittest.TestCase):
    def test_query_factory_uses_same_verified_graph(self):
        graph=NS(to_json=lambda:'{"nodes":[]}');projection=NS(visibility_digest="visible")
        project=Mock(return_value=projection);queries=Mock();queries.answer.return_value={"ok":True}
        digest="sha256:"+launcher.sha(graph.to_json().encode())
        factory=bindings.graph_query_factory(queries)(graph,projection_api=NS(project=project),source_commit="a"*40,source_tree="b"*40,expected_knowledge_sha256=digest)
        callback=factory({}, {}, {"head":"a"*40,"tree":"b"*40})
        callback(NS(kind="context",target="synthetic.txt",max_depth=0,max_bytes=8192,continuation=None))
        self.assertIs(project.call_args.args[0],graph)
        with self.assertRaises(bindings.BindingRefusal):
            bindings.graph_query_factory(queries)(graph,projection_api=NS(project=project),source_commit="a"*40,source_tree="b"*40,expected_knowledge_sha256="sha256:"+"0"*64)


    def test_equal_misses_different_honors_have_different_digest(self):
        private={"gold":{"obligations":[{"id":"S1"},{"id":"S2"}],"rubric":{},"forbidden":[]}}
        run=NS(answer="alpha beta",edits=(),spec=NS(case_type="investigation",id="synthetic"))
        class Score:
            def __init__(self,*args,**kwargs):self.judgment_digest=kwargs["judgment_digest"];self.missed_count=args[3]
        def payload(flags,reason):
            return json.dumps({"honors":[{"id":key,"honored":flag,"evidence_quote":word,"reason":reason} for key,flag,word in zip(("S1","S2"),flags,("alpha","beta"))],"false_obligations":0,"unsupported_claims":0,"patch_constraints_honored":True,"critical_authority_claim":False,"critical_patch_regression":False})
        left,_=launcher.parse_blind_score(payload((True,False),"alpha constraint"),private,run,NS(Score=Score),"anon","left",None)
        right,_=launcher.parse_blind_score(payload((False,True),"beta constraint"),private,run,NS(Score=Score),"anon","right",None)
        same,_=launcher.parse_blind_score(payload((True,False),"wording may differ"),private,run,NS(Score=Score),"anon","right",None)
        self.assertEqual(left.missed_count,right.missed_count);self.assertNotEqual(left.judgment_digest,right.judgment_digest);self.assertEqual(left.judgment_digest,same.judgment_digest)


    def test_unqualified_or_unstructured_failure_is_not_cacheable(self):
        cache=object.__new__(adapter.ValidatedAcceptanceCache)
        with self.assertRaises(adapter.AcceptanceAdapterError):
            cache.store("0"*64,{"status":"failed","isolation_verified":True})
        with self.assertRaises(adapter.AcceptanceAdapterError):
            cache.store("0"*64,{"status":"failed","classification":"unavailable","isolation_verified":True,"family_binding_digest":"0"*64})


    def test_family_protocol_distinguishes_missing_deps_exec_and_assertion(self):
        for prerequisite_status,execution_status,classification,expected in (("unavailable","failed","assertion-failed","unavailable"),("qualified","unavailable","unavailable","unavailable"),("qualified","failed","unavailable","unavailable"),("qualified","failed","assertion-failed","failed")):
            with self.subTest(prerequisite=prerequisite_status,classification=classification):
                family=NS(family="synthetic-family");config=NS(require_rust=False)
                proof=NS(public=lambda:{"status":"verified","digest":"1"*64})
                prerequisite=NS(status=prerequisite_status,family=family.family,digest="3"*64,binding_digest="4"*64,public=lambda:{"status":prerequisite_status,"digest":"3"*64})
                execution=NS(classification=classification,prerequisite_digest=prerequisite.digest,public=lambda:{"status":execution_status,"digest":"5"*64})
                api=NS(probe=Mock(return_value=proof),qualify_family=Mock(return_value=prerequisite),execute=Mock(return_value=execution))
                stager=Mock();stager.prerequisite_stage.return_value=(NS(),config,family);stager.configuration.return_value=config
                stager.cache_identity.return_value={key:"6"*64 for key in ("source_bytes_modes_sha256","grader_bytes_modes_sha256","image_sha256","toolchain_runtime_dependencies_sha256","environment_policy_sha256")}
                cache=Mock();cache.load.return_value=None;cache.store.return_value={"sha256":"7"*64}
                runner=adapter.IsolatedAcceptance(isolation_api=api,stager=stager,preflight_config=None,source_commit="a"*40,source_tree="b"*40,cache=cache)
                case={"id":"synthetic","acceptance":{"artifacts":[]},"frozen_metadata":{},"source_projections":{},"allowed_edit_paths":[]}
                result=runner.evaluate(case,{},({"argv":("python",),"cwd":"/work","environment":{}},))
                self.assertEqual(result["status"],expected)
                if expected=="unavailable":cache.store.assert_not_called()
                else:cache.store.assert_called_once()
                if prerequisite_status=="unavailable":api.execute.assert_not_called()


    def test_inert_grader_recognition_refuses_candidate_execution(self):
        import hashlib
        raw=byte_driver._TEMPLATE.replace('"DIGEST"','"'+"0"*64+'"').encode()
        self.assertEqual(byte_driver.recognize(raw,hashlib.sha256(raw).hexdigest())["expected_sha256"],"0"*64)
        malicious=raw+b'\n__import__("candidate_module")\n'
        with self.assertRaises(byte_driver.UnsupportedFamily):byte_driver.recognize(malicious,hashlib.sha256(malicious).hexdigest())


    def test_token_decision_evidence_has_numeric_only_allowlisted_projection(self):
        for decision in ("retain","inconclusive","veto"):
            analysis={"planned_runs":108,"completed_runs":108,"all_runs_completed":True,"total_tokens":None if decision=="inconclusive" else 900,"charged_tokens":1000,"raw_observations":[{"answer":"PRIVATE SENTINEL"}],"per_case":{},"per_cohort":{},"comparisons":{},"tokens":{"decision":decision,"eligible_cases":[] if decision=="inconclusive" else ["synthetic"],"eligible_per_cohort":{"routing":0 if decision=="inconclusive" else 3,"receipt":3},"median_total_tokens":{"B":None if decision=="inconclusive" else 100,"C":None if decision=="inconclusive" else 75},"relative_saving":None if decision=="inconclusive" else .25,"relative_saving_interval":None if decision=="inconclusive" else [.2,.3]},"uncertainty":{"method":"case-clustered cohort-stratified percentile bootstrap","samples":2000,"seed":7}}
            public=launcher.public_numeric_analysis(analysis,{"synthetic"})
            self.assertEqual(public["tokens"]["decision"],decision);self.assertIn("eligible_count",public["tokens"]);self.assertIn("median_total_tokens",public["tokens"]);self.assertIn("relative_saving_interval",public["tokens"]);self.assertEqual(public["uncertainty"]["samples"],2000)
            self.assertNotIn("PRIVATE SENTINEL",json.dumps(public));self.assertNotIn("raw_observations",public)


    def test_nested_grader_layout_preserves_export_dispatch_and_frozen_argv(self):
        import hashlib, os, tempfile
        from pathlib import Path
        payload=b"synthetic candidate bytes"
        expected=hashlib.sha256(payload).hexdigest()
        raw=byte_driver._TEMPLATE.replace('"DIGEST"','"'+expected+'"').encode()
        original={"filename":"check.py","content":raw,"sha256":hashlib.sha256(raw).hexdigest()}
        commands=({"argv":("/usr/bin/python3","/grader/synthetic/nested/check.py","/work"),"cwd":"/work","environment":{}},)
        layout=adapter.artifact_layout((original,),commands)
        self.assertEqual(layout[0]["relative_path"],"synthetic/nested/check.py")
        self.assertEqual(layout[0]["content"],raw);self.assertNotIn("relative_path",original)
        root=Path(__file__).resolve().parents[2]/"target/agent-fixtures/.hermes/hdr-collaboration/workspaces/study-compatibility";root.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=root) as directory:
            stager=object.__new__(bindings.SourceStager)
            stager.root=Path(directory);stager.admission=NS(assert_current=Mock())
            stager.baseline=NS(commit="a"*40,tree="b"*40,materialize=lambda destination:None)
            stager.family_recipes=None;stager.byte_driver_api=byte_driver;stager.python="/usr/bin/python3"
            stager.driver_path=Path(byte_driver.__file__);stager.driver_sha256=hashlib.sha256(stager.driver_path.read_bytes()).hexdigest();stager.seeder=None;stager.paired=None;stager.execution_options={}
            stager.api=NS(TrustedSourceFile=lambda path,digest:NS(path=path,sha256=digest),FamilyPlan=lambda **values:NS(**values))
            spec={"id":"synthetic","source_commit":"a"*40,"source_tree":"b"*40,"source_projections":{},"source_transform":None,"source_transform_target":None,"allowed_edit_paths":[]}
            handle=stager.prepare(spec,{},layout)
            self.assertEqual((handle.grader/"synthetic/nested/check.py").read_bytes(),raw)
            self.assertFalse((handle.grader/"check.py").exists())
            self.assertTrue(stager.supports_family(layout,commands))
            family=stager.family_plan(handle,commands)
            self.assertEqual(family.acceptance_argv,(commands[0]["argv"],))
            self.assertEqual(family.prerequisite_argv,(commands[0]["argv"],))
            stager.close(handle)
        context={"family":"immutable-python-inert-byte-sha256-v1","plan_digest":"1"*64,"prerequisite_digest":"2"*64,"baseline_source_digest":"3"*64}
        with patch.object(byte_driver,"read_nofollow",side_effect=(raw,payload)) as read:
            result=byte_driver.run("acceptance",list(commands[0]["argv"]),context)
        self.assertEqual(result["outcome"],"passed");self.assertTrue(result["completed"])
        self.assertEqual(read.call_args_list[0].args,("/grader","synthetic/nested/check.py"))
        self.assertEqual(commands[0]["argv"],("/usr/bin/python3","/grader/synthetic/nested/check.py","/work"))


    def test_grader_relative_contract_rejects_unsafe_and_ambiguous_layouts(self):
        validators=(adapter.grader_relative,bindings.grader_relative,byte_driver.grader_relative)
        errors=(adapter.AcceptanceAdapterError,bindings.BindingRefusal,byte_driver.UnsupportedFamily)
        for value in ("check.py","synthetic/nested/check.py"):
            for validate in validators:self.assertEqual(validate(value),value)
        for value in ("",".","/absolute.py","../escape.py","x/../escape.py","x//check.py","x/./check.py","./check.py","x\\check.py","x\x00.py","x/\ncheck.py"):
            for validate in validators:
                with self.subTest(value=value,validator=validate.__module__):
                    with self.assertRaises(errors):validate(value)
        artifact={"filename":"check.py","content":b"inert","sha256":"0"*64}
        with self.assertRaises(adapter.AcceptanceAdapterError):
            adapter.artifact_layout((artifact,),({"argv":("python","/grader/../check.py")},))
        with self.assertRaises(adapter.AcceptanceAdapterError):
            adapter.artifact_layout((artifact,),({"argv":("python","/grader/one/check.py","/grader/two/check.py")},))
        context={"family":"immutable-python-inert-byte-sha256-v1","plan_digest":"1"*64,"prerequisite_digest":"2"*64,"baseline_source_digest":"3"*64}
        with patch.object(byte_driver,"read_nofollow") as read:
            result=byte_driver.run("acceptance",["python","/grader/../check.py","/work"],context)
        self.assertEqual(result["outcome"],"unavailable");read.assert_not_called()


    def test_nested_grader_io_refuses_symlink_escape(self):
        import os, tempfile
        from pathlib import Path
        root=Path(__file__).resolve().parents[2]/"target/agent-fixtures/.hermes/hdr-collaboration/workspaces/study-compatibility";root.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=root) as directory:
            base=Path(directory);grader=base/"grader";outside=base/"outside"
            grader.mkdir(mode=0o700);outside.mkdir(mode=0o700)
            sentinel=outside/"check.py";sentinel.write_bytes(b"synthetic sentinel")
            (grader/"nested").symlink_to(outside,target_is_directory=True)
            with self.assertRaises((OSError,bindings.BindingRefusal)):
                bindings.write_grader(grader,"nested/check.py",b"must never replace sentinel")
            self.assertEqual(sentinel.read_bytes(),b"synthetic sentinel")
            with self.assertRaises((OSError,byte_driver.UnsupportedFamily)):
                byte_driver.read_nofollow(grader,"nested/check.py")
