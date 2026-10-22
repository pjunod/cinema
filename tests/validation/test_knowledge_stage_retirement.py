"""Synthetic retirement boundaries only; no cases, provider or container calls."""
import hashlib
import json
import os
from pathlib import Path
from types import SimpleNamespace as NS
import tempfile
import unittest
from unittest.mock import Mock, patch
import uuid

from validation.knowledge import evaluation_isolation as isolation
from validation.knowledge.study_driver import isolation_adapter as adapter
from validation.knowledge.study_driver import operator_workflow as operator
from validation.knowledge.study_driver import request_journal as checkpoints
from validation.knowledge.study_driver import runtime_bindings as bindings
from validation.knowledge.study_driver import trusted_launcher as launcher


class StageRetirementTests(unittest.TestCase):
    def setUp(self):
        fixture=Path(__file__).resolve().parents[2]/"target/agent-fixtures/.hermes/hdr-collaboration/workspaces/stage-retirement"
        fixture.mkdir(parents=True,exist_ok=True)
        temporary=tempfile.TemporaryDirectory(dir=fixture)
        self.addCleanup(temporary.cleanup)
        self.base=Path(temporary.name)
        self.execution=self.base/"execution";self.execution.mkdir(mode=0o700)
        self.output=self.base/"output";self.output.mkdir(mode=0o700)
        baseline=NS(commit="a"*40,tree="b"*40,materialize=lambda root:(root/"synthetic.txt").write_text("source bytes\n"))
        self.stager=bindings.SourceStager(baseline=baseline,isolation_api=isolation,owned_root=self.execution,
            engine=Path("/synthetic/engine"),endpoint="unix:///synthetic/engine.sock",image="sha256:"+"c"*64,
            python_executable="/usr/bin/python3",rustc_executable="/synthetic/rustc",cargo_executable="/synthetic/cargo",
            admission=NS(assert_current=lambda:None))
        for name in ("_ISSUED","_STEPS","_QUALIFIED"):
            self.enterContext(patch.object(isolation,name,{}))
        self.enterContext(patch.object(isolation,"_ACTIVE_ROOTS",set()))
        self.enterContext(patch.object(launcher,"_ATOMIC_PRIVATE",checkpoints.atomic_private))
        # Unit fixture models a distinct execution volume without mounting one.
        # Production has no bypass: bind_retirement_evidence checks observed dev.
        actual=bindings.SourceStager._directory_identity
        def observed(path):
            identity=actual(path)
            return (identity[0]+1,identity[1]) if Path(path)==self.output or self.output in Path(path).parents or Path(path)==self.base/"journal" else identity
        self.enterContext(patch.object(self.stager,"_directory_identity",side_effect=observed))
        self.journal=checkpoints.PrivateRequestJournal(self.base/"journal","sha256:"+"d"*64)
        self.runner=adapter.IsolatedAcceptance(isolation_api=isolation,stager=self.stager,preflight_config=None,
            source_commit="a"*40,source_tree="b"*40,cache=Mock())
        self.runner.bind_retirement(self.output,self.journal)

    def stage(self,*,cleanup=True,pinned=False):
        spec={"id":"synthetic","source_commit":"a"*40,"source_tree":"b"*40,"source_projections":{},
              "source_transform":None,"source_transform_target":None,"allowed_edit_paths":[]}
        handle=self.stager.prepare(spec,{},())
        (handle.runtime/"target"/"synthetic-build").write_bytes(b"reconstructible target")
        private=handle.root/".isolation";private.mkdir(mode=0o700)
        body={"status":"verified" if cleanup else "unavailable","cleanup_proved":cleanup,"synthetic":True}
        path=private/("probe-"+uuid.uuid4().hex+".json")
        digest=isolation._write_private(path,body)
        proof=isolation.ProbeReceipt(body["status"],digest,path,"e"*64)
        if cleanup:isolation._ISSUED[id(proof)]=(proof,body)
        self.stager.record_receipt(handle,proof)
        self.stager._stage_record(handle)["pinned"]=pinned
        self.stager.close(handle)
        return handle,proof

    def pending(self,handle,proof):
        receipt={"status":"passed","isolation_verified":True,"receipt_sha256":proof.digest}
        self.runner._pending_retirements[id(receipt)]={"receipt":receipt,"handle":handle,"proof_digests":(proof.digest,),
                                                      "object_sha256":adapter.digest(adapter.canonical(receipt))}
        return receipt

    def checkpoint(self,receipt,run_id="synthetic-run"):
        digest=checkpoints.atomic_private(self.output,"acceptance-"+launcher.sha(run_id.encode())+".private.json",receipt,replace=False)
        checkpoint=self.journal.append({"event":"acceptance_finished","run_id":run_id,"receipt_sha256":digest})
        return {"run_id":run_id,"receipt_sha256":digest,"checkpoint_sha256":checkpoint}

    def test_close_retains_scratch_then_checkpointed_retirement_archives_exact_evidence(self):
        handle,proof=self.stage();receipt=self.pending(handle,proof)
        original=proof.private_path.read_bytes()
        self.assertTrue(handle.candidate.is_dir())
        self.assertTrue((handle.runtime/"target"/"synthetic-build").is_file())
        (self.output/"answers.private.json").write_bytes(b"immutable synthetic answer")
        (handle.runtime/"candidate-fake.log").write_bytes(b"not trusted evidence")
        launcher.checkpoint_acceptance(self.output,self.journal,self.runner,"synthetic-run",receipt)
        self.assertTrue(handle.retired)
        self.assertFalse(handle.candidate.exists());self.assertFalse(handle.runtime.exists())
        archive=self.output/"stage-evidence"/handle.root.name
        self.assertEqual((archive/proof.private_path.name).read_bytes(),original)
        self.assertEqual(proof.private_path.read_bytes(),original)
        self.assertEqual((self.output/"answers.private.json").read_bytes(),b"immutable synthetic answer")
        self.assertEqual(set(p.name for p in archive.iterdir()),
            {proof.private_path.name,"stage-closed.json","archive.json","retirement-completed.json"})
        manifest=json.loads((archive/"archive.json").read_bytes())
        self.assertEqual(manifest["files"][proof.private_path.name]["sha256"],hashlib.sha256(original).hexdigest())
        self.assertEqual(json.loads((archive/"retirement-completed.json").read_bytes())["authorization"]["checkpoint_sha256"],self.journal.previous)

    def test_real_atomic_checkpoint_file_digest_is_distinct_from_object_integrity(self):
        handle,proof=self.stage();receipt=self.pending(handle,proof)
        object_sha=self.runner._pending_retirements[id(receipt)]["object_sha256"]
        fields=self.checkpoint(receipt,"newline-checkpoint")
        path=self.output/("acceptance-"+launcher.sha(b"newline-checkpoint")+".private.json")
        raw=path.read_bytes()
        self.assertTrue(raw.endswith(b"\n"))
        self.assertEqual(fields["receipt_sha256"],hashlib.sha256(raw).hexdigest())
        self.assertNotEqual(fields["receipt_sha256"],object_sha)
        self.runner.retire_acceptance(receipt,**fields)
        self.assertTrue(handle.retired)
        complete=json.loads((self.output/"stage-evidence"/handle.root.name/"retirement-completed.json").read_bytes())
        self.assertEqual(complete["authorization"]["receipt_sha256"],fields["receipt_sha256"])

    def test_internal_build_hardlinks_can_retire_without_outside_aliases(self):
        handle,proof=self.stage();receipt=self.pending(handle,proof)
        original=handle.runtime/"target"/"synthetic-build"
        os.link(original,handle.runtime/"target"/"internal-build-alias")
        launcher.checkpoint_acceptance(self.output,self.journal,self.runner,"internal-links",receipt)
        self.assertTrue(handle.retired);self.assertFalse(handle.runtime.exists())

    def test_receipt_or_journal_write_failure_never_calls_retirement(self):
        runner=Mock();journal=Mock();receipt={"status":"passed"}
        with patch.object(launcher,"save_private",side_effect=OSError("synthetic receipt failure")):
            with self.assertRaises(OSError):launcher.checkpoint_acceptance(self.output,journal,runner,"synthetic",receipt)
        journal.append.assert_not_called();runner.retire_acceptance.assert_not_called()
        journal.append.side_effect=OSError("synthetic journal failure")
        with self.assertRaises(OSError):launcher.checkpoint_acceptance(self.output,journal,runner,"synthetic",receipt)
        runner.retire_acceptance.assert_not_called()

    def test_unknown_receipt_or_wrong_checkpoint_event_retains_stage(self):
        handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt)
        with self.assertRaises(adapter.AcceptanceAdapterError):self.runner.retire_acceptance(dict(receipt),**fields)
        wrong=dict(fields,run_id="other-run")
        with self.assertRaises((adapter.AcceptanceAdapterError,FileNotFoundError)):self.runner.retire_acceptance(receipt,**wrong)
        self.assertTrue(handle.candidate.exists());self.assertFalse(handle.retired)
        with self.assertRaises(adapter.AcceptanceAdapterError):self.runner.evaluate({}, {}, ())

    def test_changed_checkpoint_bytes_retains_stage(self):
        handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt)
        path=self.journal.root/f"{self.journal.sequence-1:08d}.private.json"
        path.write_bytes(path.read_bytes()+b" ")
        with self.assertRaisesRegex(adapter.AcceptanceAdapterError,"RetirementCheckpointChanged"):
            self.runner.retire_acceptance(receipt,**fields)
        self.assertTrue(handle.runtime.exists())

    def test_unproved_cleanup_and_unrecorded_evidence_cannot_retire(self):
        handle,proof=self.stage(cleanup=False);receipt=self.pending(handle,proof);fields=self.checkpoint(receipt)
        with self.assertRaisesRegex(bindings.BindingRefusal,"StageContainerCleanupUnproved"):
            self.runner.retire_acceptance(receipt,**fields)
        self.assertTrue(handle.candidate.exists())
        extra=handle.root/".isolation"/("probe-"+uuid.uuid4().hex+".json")
        bindings.durable(extra,{"cleanup_proved":True})
        with self.assertRaisesRegex(bindings.BindingRefusal,"UnrecordedIsolationEvidence"):
            self.stager.retirement_evidence(handle)

    def test_qualified_baseline_stays_pinned_until_verified_terminal_checkpoint(self):
        handle,proof=self.stage(pinned=True);receipt=self.pending(handle,proof);fields=self.checkpoint(receipt)
        with self.assertRaisesRegex(bindings.BindingRefusal,"QualifiedBaselineStillPinned"):
            self.runner.retire_acceptance(receipt,**fields)
        self.assertTrue(handle.runtime.exists())
        # This fixture is a baseline, so remove its synthetic acceptance handle.
        self.runner._pending_retirements.clear();self.runner.prequalified["synthetic"]=(None,None,handle)
        result_sha=checkpoints.atomic_private(self.output,"result.safe.json",{"status":"complete"},replace=False)
        terminal_sha=self.journal.finish("complete",result_sha)
        self.runner.retire_completed_study(result_sha256=result_sha,checkpoint_sha256=terminal_sha)
        self.assertTrue(handle.retired);self.assertFalse(self.runner.prequalified)
        with self.assertRaisesRegex(adapter.AcceptanceAdapterError,"AcceptanceAfterTerminalStudy"):
            self.runner.evaluate({}, {}, ())

    def test_inconclusive_terminal_retains_qualified_baseline(self):
        handle,_=self.stage(pinned=True)
        result_sha=checkpoints.atomic_private(self.output,"result.safe.json",{"status":"inconclusive"},replace=False)
        terminal_sha=self.journal.finish("inconclusive",result_sha)
        with self.assertRaisesRegex(adapter.AcceptanceAdapterError,"RetirementCheckpointEventMismatch"):
            self.runner.retire_completed_study(result_sha256=result_sha,checkpoint_sha256=terminal_sha)
        self.assertTrue(handle.candidate.exists());self.assertFalse(handle.retired)

    def test_scratch_child_links_are_unlinked_without_touching_outside_targets(self):
        for attack in ("symlink","hardlink"):
            with self.subTest(attack=attack):
                handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt,attack)
                outside=self.base/("sentinel-"+attack);outside.write_bytes(b"retained")
                if attack=="symlink":(handle.runtime/"alias").symlink_to(outside)
                else:os.link(outside,handle.runtime/"alias")
                self.runner.retire_acceptance(receipt,**fields)
                self.assertTrue(handle.retired);self.assertEqual(outside.read_bytes(),b"retained")

    def test_registered_root_inode_swap_or_symlink_is_refused(self):
        for attack in ("swap","symlink"):
            with self.subTest(attack=attack):
                handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt,attack)
                handle.runtime.rename(handle.root/"moved-runtime")
                if attack=="swap":handle.runtime.mkdir(mode=0o700)
                else:handle.runtime.symlink_to(handle.root/"moved-runtime",target_is_directory=True)
                with self.assertRaises(bindings.BindingRefusal):self.runner.retire_acceptance(receipt,**fields)
                self.assertTrue(handle.candidate.exists());self.assertFalse(handle.retired)
                self.runner._pending_retirements.clear()

    def test_source_or_receipt_identity_change_refuses_deletion(self):
        for attack in ("source","receipt"):
            with self.subTest(attack=attack):
                handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt,attack)
                if attack=="source":(handle.candidate/"synthetic.txt").write_text("changed")
                else:
                    raw=proof.private_path.read_bytes();replacement=proof.private_path.parent/"replacement.tmp"
                    replacement.write_bytes(raw);replacement.chmod(0o600);os.replace(replacement,proof.private_path)
                with self.assertRaises(bindings.BindingRefusal):self.runner.retire_acceptance(receipt,**fields)
                self.assertTrue(handle.runtime.exists());self.runner._pending_retirements.clear()

    def test_archive_bound_or_copy_failure_never_deletes_scratch(self):
        handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt)
        with patch.object(self.stager,"RECEIPT_TOTAL_MAX",1):
            with self.assertRaisesRegex(bindings.BindingRefusal,"RetirementEvidenceBoundExceeded"):
                self.runner.retire_acceptance(receipt,**fields)
        self.assertTrue(handle.candidate.exists());self.assertFalse(handle.retired)
        self.runner._pending_retirements.clear()
        handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt,"copy-failure")
        with patch.object(bindings.os,"write",side_effect=OSError("synthetic archive failure")):
            with self.assertRaises(OSError):self.runner.retire_acceptance(receipt,**fields)
        self.assertTrue(handle.candidate.exists());self.assertTrue(proof.private_path.exists())
        self.assertFalse(handle.retired)

    def test_removal_failure_has_external_proof_and_cannot_resume(self):
        handle,proof=self.stage();receipt=self.pending(handle,proof);fields=self.checkpoint(receipt)
        with patch.object(bindings.shutil,"rmtree",side_effect=OSError("synthetic removal failure")):
            with self.assertRaises(OSError):self.runner.retire_acceptance(receipt,**fields)
        archive=self.output/"stage-evidence"/handle.root.name
        self.assertTrue((archive/"archive.json").is_file())
        self.assertTrue((self.output/"stage-evidence"/(handle.root.name+"-retirement-failed.json")).is_file())
        self.assertFalse((archive/"retirement-completed.json").exists())
        self.assertTrue(handle.candidate.exists());self.assertFalse(handle.retired)
        with self.assertRaises(adapter.AcceptanceAdapterError):self.runner.evaluate({}, {}, ())

    def test_forged_stage_and_unissued_probe_are_refused(self):
        handle,proof=self.stage()
        forged=bindings.Stage(handle.root,handle.candidate,handle.grader,handle.runtime)
        with self.assertRaisesRegex(bindings.BindingRefusal,"UnknownOrRetiredStage"):self.stager.close(forged)
        copied=isolation.ProbeReceipt(proof.status,proof.digest,proof.private_path,proof.config_digest)
        with self.assertRaisesRegex(bindings.BindingRefusal,"UnissuedIsolationReceipt"):
            self.stager.record_receipt(handle,copied)

    def test_same_device_evidence_root_is_not_an_execution_quota_escape(self):
        self.stager.__dict__.pop("_retirement_evidence_root")
        elsewhere=self.base/"same-device";elsewhere.mkdir(mode=0o700)
        with self.assertRaisesRegex(bindings.BindingRefusal,"ExternalRetirementEvidenceRequired"):
            self.stager.bind_retirement_evidence(elsewhere,journal_root=self.journal.root)


class TerminalOperatorRetirementTests(unittest.TestCase):
    def test_terminal_cleanup_failure_preserves_complete_result_and_journal(self):
        fixture=Path(__file__).resolve().parents[2]/"target/agent-fixtures/.hermes/hdr-collaboration/workspaces/stage-retirement"
        fixture.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=fixture) as directory:
            root=Path(directory);output=root/"output";output.mkdir(mode=0o700)
            study=object.__new__(operator.PreparedStudy)
            study.calibrated=True;study.journal=None;study.output=output
            study.spec={"study_journal":str(root/"journal")};study.config={"manifest":NS(sha256="sha256:"+"a"*64)}
            study.admission=NS(assert_current=lambda:None)
            for name in ("graph","client","baseline","gate","nav"):setattr(study,name,None)
            def cleanup(**kwargs):
                self.assertTrue(study.journal.terminal)
                self.assertTrue((output/"result.safe.json").is_file())
                self.assertEqual(study.journal.previous,kwargs["checkpoint_sha256"])
                raise OSError("synthetic terminal cleanup failure")
            study.runner=NS(retire_completed_study=Mock(side_effect=cleanup))
            with patch.object(operator.launcher,"launch",return_value={"status":"complete"}):
                with self.assertRaises(OSError):study.run()
            self.assertEqual(json.loads((output/"result.safe.json").read_bytes()),{"status":"complete"})
            events=[json.loads(p.read_bytes())["payload"] for p in sorted(study.journal.root.iterdir())]
            self.assertEqual([event["event"] for event in events],["journal_created","journal_finished"])
            failure=json.loads((output/"retirement-failure.safe.json").read_bytes())
            self.assertFalse(failure["cleanup_proved"])
            self.assertEqual(failure["terminal_checkpoint_sha256"],study.journal.previous)
            study.runner.retire_completed_study.assert_called_once()
