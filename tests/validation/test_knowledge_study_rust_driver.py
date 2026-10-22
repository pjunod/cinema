"""Synthetic observations for the protected Rust framework outcome boundary."""
from types import SimpleNamespace as NS
import unittest
from unittest.mock import patch

from validation.knowledge.study_driver import rust_driver_factory as factory


def protected_namespace():
    namespace={"_runtime":NS(),"EXPECTED_SCHEMA":"synthetic","rpc":lambda *a:None}
    exec(compile(factory.PROTECTED,"synthetic-protected-driver","exec",dont_inherit=True),namespace)
    return namespace


def summary(status="ok",passed=1,failed=0,ignored=0):
    return f"test result: {status}. {passed} passed; {failed} failed; {ignored} ignored; 0 measured; 7 filtered out; finished in 0.01s\n"


def observed(returncode=0):
    status="ok" if returncode==0 else f"exit {returncode}"
    return NS(returncode=returncode,stdout=f"── {status} in 1s · full log: target/quiet/cargo-test-locked-20260101-000000.log ──\n".encode())


class RustFrameworkBoundaryTests(unittest.TestCase):
    def test_actual_terminal_success_has_positive_framework_counts(self):
        namespace=protected_namespace();raw=("running 1 test\ntest synthetic ... ok\n\n"+summary()).encode()
        with patch.dict(namespace,{"_read_log":lambda path:raw}):
            completed,failed,digest=namespace["framework_counts"](observed())
        self.assertEqual((completed,failed),(1,0));self.assertEqual(len(digest),64)

    def test_candidate_reason_cannot_replace_terminal_framework_failure(self):
        namespace=protected_namespace()
        raw=("running 1 test\nassertion failed; untrusted returned reason:\n"+summary()+"\nfailures:\n synthetic\n\n"+summary("FAILED",0,1)+"\nerror: test failed, to rerun pass `-p synthetic --lib`\n").encode()
        with patch.dict(namespace,{"_read_log":lambda path:raw}):
            completed,failed,_=namespace["framework_counts"](observed(101))
        self.assertEqual((completed,failed),(1,1))

    def test_zero_checks_exit_mismatch_and_extra_tail_never_pass(self):
        namespace=protected_namespace()
        values=(("running 0 tests\n"+summary(passed=0),observed()),
            ("running 1 test\n"+summary(),observed(101)),
            ("running 1 test\n"+summary()+"untrusted extra output\n",observed()))
        for text,result in values:
            with self.subTest(text=text),patch.dict(namespace,{"_read_log":lambda path:text.encode()}):
                with self.assertRaises(ValueError):namespace["framework_counts"](result)

    def test_wrapper_path_cannot_escape_protected_quiet_logs(self):
        namespace=protected_namespace()
        result=NS(returncode=0,stdout="── ok in 1s · full log: target/quiet/../forged.log ──\n".encode())
        with patch.dict(namespace,{"_read_log":lambda path:self.fail("unsafe log was read")}):
            with self.assertRaises(ValueError):namespace["framework_counts"](result)
