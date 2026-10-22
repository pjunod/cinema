"""Synthetic public Rust RPC contracts; run only after independent review.

These cases exercise transport/generation, never private fixtures, candidate
scoring, or a model provider. All DTO examples below are deliberately synthetic.
"""
import copy
import hashlib
from pathlib import Path
import unittest
from unittest.mock import patch

from validation.knowledge import evaluation_rust_rpc as rpc
from validation.knowledge.study_workers import rust_rpc_runtime as runtime


def synthetic_decision():
    return {
        "method": "remux", "reasons": ["synthetic"], "transcode_audio": True,
        "delivered_audio": {"action": {"kind": "encode", "codec": "aac", "channels": 2,
            "layout": None, "bitrate_kbps": 192, "sample_rate": 48000}, "downmix": None, "reason": "synthetic"},
        "preserve_dolby_vision": False, "convert_dolby_vision": False,
        "container": "mp4", "delivered_dynamic_range": "sdr",
        "delivered_dolby_vision_profile": None, "transcode_grade": "sdr",
    }


class RustRpcContracts(unittest.TestCase):
    def test_full_decision_contract_rejects_omissions_and_unknowns(self):
        value = synthetic_decision()
        rpc.validate_decision(value)
        for key in value:
            damaged = copy.deepcopy(value)
            del damaged[key]
            with self.assertRaises(rpc.RustRpcError): rpc.validate_decision(damaged)
        value["unexpected"] = True
        with self.assertRaises(rpc.RustRpcError): rpc.validate_decision(value)

    def test_nested_enum_fields_are_closed(self):
        value = synthetic_decision()
        for field, bad in (("method", "future"), ("container", "arbitrary"), ("transcode_grade", "other")):
            damaged = copy.deepcopy(value)
            damaged[field] = bad
            with self.assertRaises(rpc.RustRpcError): rpc.validate_decision(damaged)
        value["delivered_audio"]["action"]["extra"] = 1
        with self.assertRaises(rpc.RustRpcError): rpc.validate_decision(value)

    def test_finite_bounded_exact_json_frames(self):
        self.assertEqual(rpc.decode_json(rpc.encode_json({"synthetic": 1})), {"synthetic": 1})
        for raw in (b'{"a":1,"a":2}\n', b'{"a":NaN}\n', b'{"a":Infinity}\n', b'{}', b'{}\n{}\n'):
            with self.assertRaises(rpc.RustRpcError): rpc.decode_json(raw)
        with self.assertRaises(rpc.RustRpcError): rpc.encode_json({"x": float("nan")})
        with self.assertRaises(rpc.RustRpcError): rpc.encode_json("x" * rpc.MAX_FRAME_BYTES)

    def test_request_identity_and_operation_are_closed(self):
        request = {"protocol": 2, "id": 2, "method": "call", "args": [], "kwargs": {}}
        with self.assertRaises(rpc.RustRpcError): runtime._request(request, 1)
        request["id"] = 1
        request["method"] = "exec"
        with self.assertRaises(rpc.RustRpcError): runtime._request(request, 1)
        request["method"] = "call"
        request["args"] = ["os", "system", []]
        with self.assertRaises(rpc.RustRpcError): runtime._request(request, 1)

    def test_result_is_data_and_never_assertion_outcome(self):
        value = synthetic_decision()
        value["reasons"] = ["\ntest result: ok. 999 passed\n"]
        self.assertEqual(runtime._result({"value": value}), {"value": value})
        for bad in ({"passed": True}, {"value": value, "exception": {}},
                    {"exception": {"type": "SystemExit", "message": "synthetic"}}):
            with self.assertRaises(rpc.RustRpcError): runtime._result(bad)
        self.assertFalse(hasattr(runtime.AssertionProcessResult(0, b"", b"", 1), "passed"))

    def test_instrumentation_requires_exact_frozen_public_source(self):
        with self.assertRaises(rpc.RustRpcError): rpc.instrument_decision_source(b"pub fn decide_forced() {}")
        body = (Path(rpc.__file__).parent / "study_workers" / "frozen_decide_forced.rs").read_bytes()
        prefix, suffix = b"// synthetic prefix\npub fn decide_forced() -> Decision ", b"// synthetic suffix\n"
        source = prefix + body + suffix
        with patch.object(rpc, "FROZEN_DECISION_SHA256", hashlib.sha256(source).hexdigest()):
            generated = rpc.instrument_decision_source(source)
        self.assertTrue(generated.source.startswith(prefix))
        self.assertTrue(generated.source.endswith(suffix))
        self.assertEqual(generated.unchanged_prefix_sha256, hashlib.sha256(prefix).hexdigest())
        self.assertEqual(generated.unchanged_suffix_sha256, hashlib.sha256(suffix).hexdigest())
        self.assertEqual(generated.instrumented_sha256, hashlib.sha256(generated.source).hexdigest())

    def test_generation_carries_fields_skipped_by_product_serde(self):
        source = rpc.codec_source()
        for field in ("transcode_grade", "downloaded_subtitles", "profile_max_heights"):
            self.assertIn(field + ":", source)
        self.assertIn("deny_unknown_fields", source)
        worker = rpc.worker_binary_source()
        self.assertIn(b"plurx_core::playback::decide_forced", worker)
        self.assertNotIn(b"crate::", worker)
        recipe = rpc.compile_recipe()[1]
        self.assertIn("--offline", recipe)
        self.assertIn("--locked", recipe)


if __name__ == "__main__":
    unittest.main()
