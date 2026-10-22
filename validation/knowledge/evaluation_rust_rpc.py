"""Gold-free typed Rust decision seam for the frozen knowledge study.

Generation reads only named public frozen source files. The protected tree gets
one reviewed body replacement; the candidate tree gets an additional binary.
Compilation, isolation, assertion accounting and outcome authentication remain
controller responsibilities. No callback result here is an assertion result.
"""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import math
from pathlib import Path
from typing import Any

FROZEN_REVISION = "168fe74fb2a47f3ccd56b795741dfe3422307b0d"
FROZEN_TREE = "ea8c9c283a71b67bf773511144eb3ee7ab09ee9c"
FROZEN_DECISION_SHA256 = "f6b6de592a3185092c0c181011e8eaccd39183b472f4e34913495a1a027998f4"
DECISION_PATH = "crates/plurx-core/src/playback/mod.rs"
RPC_MODULE = "plurx_core.playback"
RPC_FUNCTION = "decide_forced"
MAX_FRAME_BYTES = 65_536
MAX_DEPTH = 16
SOCKET_PATH = "/work/.runtime/protected-rpc/endpoint.sock"

# A closed value algebra. Product types never cross an ABI or pickle boundary.
def opt(t): return ("option", t)
def vec(t): return ("vec", t)
def mapping(t): return ("map", t)
def pair(*t): return ("tuple", *t)

STRUCTS = {
    "DolbyVisionFacts": ("crate::domain::DolbyVisionFacts", {
        "profile": opt("i64"), "level": opt("i64"), "bl_compat_id": opt("i64"),
        "el_present": opt("bool"), "rpu_present": opt("bool")}),
    "AudioStream": ("crate::domain::AudioStream", {
        "index": "i64", "codec": "string", "channels": opt("i64"),
        "sample_rate": opt("i64"), "channel_layout": opt("string"),
        "language": opt("string"), "title": opt("string"), "default": "bool"}),
    "SubtitleStream": ("crate::domain::SubtitleStream", {
        "index": "i64", "codec": "string", "language": opt("string"),
        "title": opt("string"), "default": "bool", "forced": "bool", "hearing_impaired": "bool"}),
    "SubtitleExecution": ("crate::domain::SubtitleExecution", {"engine_sha256": "string", "platform": "string"}),
    "SubtitleTranscription": ("crate::domain::SubtitleTranscription", {
        "audio_index": "i64", "artifact_key": "string", "model_sha256": "string", "pipeline_digest": "string",
        "adapter": "string", "generated_at_ms": "i64", "execution": opt("SubtitleExecution")}),
    "DownloadedSubtitle": ("crate::domain::DownloadedSubtitle", {
        "source_size": "i64", "source_mtime": "i64", "provider_file_id": "i64",
        "transcription": opt("SubtitleTranscription"), "language": "string", "title": "string",
        "hearing_impaired": "bool", "forced": "bool", "vtt": "string"}),
    "MediaFile": ("crate::domain::MediaFile", {
        "id": "i64", "item_id": "i64", "path": "path", "size": "i64", "mtime": "i64",
        "duration_ms": opt("i64"), "container": opt("string"), "video_codec": opt("string"),
        "video_codec_tag": opt("string"), "field_order": opt("string"), "video_profile": opt("string"),
        "width": opt("i64"), "height": opt("i64"), "bit_depth": opt("i64"), "hdr": opt("string"),
        "hdr_format": opt("string"), "max_cll": opt("i64"), "max_fall": opt("i64"),
        "mastering_max_luminance": opt("i64"), "luminance_source": opt("string"),
        "dolby_vision": "DolbyVisionFacts", "bitrate": opt("i64"), "audio_streams": vec("AudioStream"),
        "subtitle_streams": vec("SubtitleStream"), "downloaded_subtitles": vec("DownloadedSubtitle"),
        "scanned_at": "i64", "audio_offset_ms": "i64", "probed": "bool"}),
    "AudioSink": ("crate::playback::AudioSink", {
        "codec": "string", "max_channels": "u8", "passthrough": "bool", "sample_rates_hz": vec("u32")}),
    "LearnedLimit": ("crate::playback::LearnedLimit", {
        "identity": "string", "label": "string", "lost": "i64", "secs": "i64", "rate": "f64", "at_ms": "i64"}),
    "DeviceProfile": ("crate::playback::DeviceProfile", {
        "name": "string", "description": "string", "containers": vec("string"), "video_codecs": vec("string"),
        "audio_codecs": vec("string"), "max_audio_channels": mapping("u8"), "claimed_audio_decoders": ("set", "string"),
        "audio_sink_claims": mapping("AudioSink"), "max_height": opt("i64"), "video_max_heights": mapping("i64"),
        "max_bitrate": opt("i64"), "supports_hdr": "bool", "supports_dolby_vision": "bool", "dolby_vision_profiles": vec("u8"),
        "supports_hdr10_transcode": "bool", "remux_dolby_vision": "bool", "progressive_hevc_sample_entries": opt(vec("string")),
        "presents": mapping(("set", "Transfer")), "profile_max_heights": ("pairmap", "i64"), "learned_limits": vec("LearnedLimit")}),
    "RenderCaps": ("crate::playback::RenderCaps", {
        "dv_strippable": "bool", "dolby_vision_p5_render": "bool", "hdr10_passthrough": "bool",
        "hdr10_max_height": "i64", "dolby_vision_convert": "bool"}),
    "AudioDelivery": ("crate::playback::AudioDelivery", {"action": "AudioAction", "downmix": opt("DownmixMatrix"), "reason": "string"}),
    "Decision": ("crate::playback::Decision", {
        "method": "PlaybackMethod", "reasons": vec("string"), "transcode_audio": "bool", "delivered_audio": "AudioDelivery",
        "preserve_dolby_vision": "bool", "convert_dolby_vision": "bool", "container": "Container",
        "delivered_dynamic_range": "DynamicRange", "delivered_dolby_vision_profile": opt("u8"), "transcode_grade": "OutputGrade"}),
}
ENUMS = {
    "Force": ("crate::playback::Force", {"auto": "Auto", "original": "Original", "transcode": "Transcode"}),
    "Transfer": ("crate::playback::Transfer", {"sdr": "Sdr", "pq": "Pq", "hlg": "Hlg", "unknown": "Unknown"}),
    "PlaybackMethod": ("crate::playback::PlaybackMethod", {"direct_play": "DirectPlay", "remux": "Remux", "transcode": "Transcode"}),
    "OutputGrade": ("crate::transcode::OutputGrade", {"sdr": "Sdr", "hdr10": "Hdr10"}),
    "Container": ("&'static str", {"mp4": "Mp4"}),
    "DynamicRange": ("&'static str", {"sdr": "Sdr", "hdr10": "Hdr10", "hlg": "Hlg", "dolby_vision": "DolbyVision"}),
}
TAGGED = {
    "AudioAction": ("crate::playback::AudioAction", {
        "none": ("None", {}), "copy": ("Copy", {"codec": "string", "channels": "u8"}),
        "encode": ("Encode", {"codec": "string", "channels": "u8", "layout": opt("string"), "bitrate_kbps": "u32", "sample_rate": "u32"})}),
    "DownmixMatrix": ("crate::playback::DownmixMatrix", {
        "requires_layout_measurement": ("RequiresLayoutMeasurement", {"source_channels": "u8"}),
        "lo_ro_5_1": ("LoRo51", {}), "lo_ro_7_1": ("LoRo71", {}),
        "limited_default": ("LimitedDefault", {"source_channels": "u8"})}),
}


class RustRpcError(ValueError):
    """Malformed or incompatible untrusted transport value."""


def _check(value: Any, spec: Any, depth: int = 0) -> None:
    if depth > MAX_DEPTH:
        raise RustRpcError("maximum typed value depth exceeded")
    if isinstance(spec, tuple):
        kind, *rest = spec
        if kind == "option":
            if value is not None: _check(value, rest[0], depth + 1)
        elif kind == "map":
            if type(value) is not dict or any(type(k) is not str for k in value): raise RustRpcError("expected string map")
            for item in value.values(): _check(item, rest[0], depth + 1)
        elif kind == "tuple":
            if type(value) is not list or len(value) != len(rest): raise RustRpcError("expected fixed tuple")
            for item, subtype in zip(value, rest): _check(item, subtype, depth + 1)
        else:
            if type(value) is not list: raise RustRpcError("expected list")
            subtype = pair(pair("string", "string"), rest[0]) if kind == "pairmap" else rest[0]
            for item in value: _check(item, subtype, depth + 1)
            if kind in {"set", "pairmap"}:
                keys = [json.dumps(item[0] if kind == "pairmap" else item, sort_keys=True) for item in value]
                if len(keys) != len(set(keys)): raise RustRpcError("duplicate set/map entry")
        return
    if spec in STRUCTS:
        fields = STRUCTS[spec][1]
        if type(value) is not dict or value.keys() != fields.keys(): raise RustRpcError(f"invalid {spec} fields")
        for key, subtype in fields.items(): _check(value[key], subtype, depth + 1)
    elif spec in ENUMS:
        if type(value) is not str or value not in ENUMS[spec][1]: raise RustRpcError(f"invalid {spec} enum")
    elif spec in TAGGED:
        if type(value) is not dict or type(value.get("kind")) is not str or value["kind"] not in TAGGED[spec][1]: raise RustRpcError(f"invalid {spec} tag")
        fields = TAGGED[spec][1][value["kind"]][1]
        if value.keys() != {"kind", *fields}: raise RustRpcError(f"invalid {spec} fields")
        for key, subtype in fields.items(): _check(value[key], subtype, depth + 1)
    elif spec in {"string", "path"}:
        if type(value) is not str or len(value.encode("utf-8")) > MAX_FRAME_BYTES: raise RustRpcError("invalid string")
    elif spec == "bool":
        if type(value) is not bool: raise RustRpcError("expected boolean")
    elif spec == "f64":
        if type(value) not in (float, int): raise RustRpcError("expected finite number")
        try:
            finite = math.isfinite(value)
        except OverflowError:
            finite = False
        if not finite: raise RustRpcError("expected finite number")
    elif spec in {"i64", "u8", "u32"}:
        lo, hi = {"i64": (-(2**63), 2**63 - 1), "u8": (0, 255), "u32": (0, 2**32 - 1)}[spec]
        if type(value) is not int or not lo <= value <= hi: raise RustRpcError(f"expected {spec}")
    else:
        raise RustRpcError("unknown schema type")


def validate_arguments(args: Any) -> None:
    _check(args, pair("MediaFile", "DeviceProfile", "Force", "RenderCaps"))
    encode_json(args)


def validate_decision(value: Any) -> None:
    _check(value, "Decision")
    encode_json(value)


def encode_json(value: Any) -> bytes:
    try:
        raw = json.dumps(value, ensure_ascii=True, allow_nan=False, separators=(",", ":")).encode("utf-8")
    except (ValueError, TypeError, UnicodeError) as exc:
        raise RustRpcError("invalid JSON value") from exc
    if len(raw) + 1 > MAX_FRAME_BYTES: raise RustRpcError("frame limit exceeded")
    return raw + b"\n"


def decode_json(raw: bytes) -> Any:
    if len(raw) > MAX_FRAME_BYTES or not raw.endswith(b"\n"): raise RustRpcError("invalid frame boundary")
    def pairs(items):
        out = {}
        for key, value in items:
            if key in out: raise RustRpcError("duplicate JSON key")
            out[key] = value
        return out
    try:
        return json.loads(raw, object_pairs_hook=pairs, parse_constant=lambda _: (_ for _ in ()).throw(RustRpcError("nonfinite number")))
    except (ValueError, UnicodeError, RecursionError) as exc:
        raise RustRpcError("invalid JSON frame") from exc


def schema_sha256() -> str:
    return hashlib.sha256(encode_json({"structs": STRUCTS, "enums": ENUMS, "tagged": TAGGED})).hexdigest()


def _rust_type(t):
    if isinstance(t, tuple):
        k, *r = t
        if k == "option": return f"Option<{_rust_type(r[0])}>"
        if k in {"vec", "set"}: return f"Vec<{_rust_type(r[0])}>"
        if k == "map": return f"std::collections::BTreeMap<String, {_rust_type(r[0])}>"
        if k == "pairmap": return f"Vec<((String, String), {_rust_type(r[0])})>"
        if k == "tuple": return "(" + ", ".join(map(_rust_type, r)) + ")"
    return {"string": "String", "path": "String"}.get(t, "Wire" + t if t in STRUCTS or t in ENUMS or t in TAGGED else t)


def _convert(t, expr, outward, level=0):
    if isinstance(t, tuple):
        k, *r = t
        x = f"v{level}"
        if k == "option":
            iterator = f"({expr}).as_ref()" if outward else f"({expr})"
            return f"{iterator}.map(|{x}| {_convert(r[0], x, outward, level+1)})"
        if k in {"vec", "set"}:
            iterator = "iter" if outward else "into_iter"
            return f"({expr}).{iterator}().map(|{x}| {_convert(r[0], x, outward, level+1)}).collect()"
        if k in {"map", "pairmap"}:
            iterator = "iter" if outward else "into_iter"
            key = "key.clone()" if outward else "key"
            return f"({expr}).{iterator}().map(|(key, {x})| ({key}, {_convert(r[0], x, outward, level+1)})).collect()"
    if t in STRUCTS or t in ENUMS or t in TAGGED:
        return f"Wire{t}::from({expr})" if outward else f"({expr}).into()"
    if t == "path": return f"({expr}).to_str().expect(\"non-UTF8 path is outside RPC schema\").to_owned()" if outward else f"({expr}).into()"
    return f"({expr}).clone()" if outward else expr


def codec_source() -> str:
    """Generate a closed DTO codec; no candidate source is evaluated here."""
    out = ["// Generated frozen-study value codec; no product serde defaults are used.\n"]
    for name, (product, variants) in ENUMS.items():
        out.append("#[derive(serde::Serialize, serde::Deserialize)]\nenum Wire" + name + " {\n")
        out.extend(f'#[serde(rename = "{token}")] {variant},\n' for token, variant in variants.items())
        out.append("}\n")
        pattern = "*v" if product == "&'static str" else "v"
        out.append(f"impl From<&{product}> for Wire{name} {{ fn from(v: &{product}) -> Self {{ match {pattern} {{\n")
        out.extend(f'{json.dumps(token) if product == "&\'static str" else product + "::" + variant} => Self::{variant},\n' for token, variant in variants.items())
        if product == "&'static str": out.append('_ => panic!("static string outside RPC schema"),\n')
        out.append("} } }\n")
        out.append(f"impl From<Wire{name}> for {product} {{ fn from(v: Wire{name}) -> Self {{ match v {{\n")
        out.extend(f'Wire{name}::{variant} => {json.dumps(token) if product == "&\'static str" else product + "::" + variant},\n' for token, variant in variants.items())
        out.append("} } }\n")
    for name, (product, variants) in TAGGED.items():
        out.append(f'#[derive(serde::Serialize, serde::Deserialize)]\n#[serde(tag="kind", deny_unknown_fields)]\nenum Wire{name} {{\n')
        for token, (variant, fields) in variants.items():
            out.append(f'#[serde(rename="{token}")] {variant}' + (" {" + ",".join(f"{k}: {_rust_type(t)}" for k, t in fields.items()) + "}" if fields else "") + ",\n")
        out.append("}\n")
        for outward in (True, False):
            src, dst = (product, "Wire" + name) if outward else ("Wire" + name, product)
            ref = "&" if outward else ""
            out.append(f"impl From<{ref}{src}> for {dst} {{ fn from(v: {ref}{src}) -> Self {{ match v {{\n")
            for _, (variant, fields) in variants.items():
                binding = " {" + ",".join(fields) + "}" if fields else ""
                result = " {" + ",".join(f"{k}: {_convert(t, k, outward)}" for k, t in fields.items()) + "}" if fields else ""
                out.append(f"{src}::{variant}{binding} => Self::{variant}{result},\n")
            out.append("} } }\n")
    for name, (product, fields) in STRUCTS.items():
        out.append(f"#[derive(serde::Serialize, serde::Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct Wire{name} {{\n")
        out.extend(f"{key}: {_rust_type(t)},\n" for key, t in fields.items())
        out.append("}\n")
        out.append(f"impl From<&{product}> for Wire{name} {{ fn from(v: &{product}) -> Self {{ Self {{\n")
        out.extend(f"{key}: {_convert(t, '&v.'+key, True)},\n" for key, t in fields.items())
        out.append("} } }\n")
        out.append(f"impl From<Wire{name}> for {product} {{ fn from(v: Wire{name}) -> Self {{ Self {{\n")
        out.extend(f"{key}: {_convert(t, 'v.'+key, False)},\n" for key, t in fields.items())
        out.append("} } }\n")
    return "".join(out)


@dataclass(frozen=True)
class InstrumentedSource:
    source: bytes
    original_sha256: str
    instrumented_sha256: str
    unchanged_prefix_sha256: str
    unchanged_suffix_sha256: str
    generator_sha256: str
    schema_sha256: str


def instrument_decision_source(clean_source: bytes) -> InstrumentedSource:
    """Replace the exact frozen body, preserving every byte outside it.

    The controller must additionally bind clean_source to its trusted git archive;
    digest equality alone does not authenticate who supplied source bytes.
    """
    if hashlib.sha256(clean_source).hexdigest() != FROZEN_DECISION_SHA256:
        raise RustRpcError("protected instrumentation requires the exact clean frozen public module")
    frozen_body = (Path(__file__).parent / "study_workers" / "frozen_decide_forced.rs").read_bytes()
    needle = b"pub fn decide_forced("
    if clean_source.count(needle) != 1 or clean_source.count(frozen_body) != 1:
        raise RustRpcError("clean frozen decision body not uniquely identified")
    signature_start = clean_source.index(needle)
    body_start = clean_source.find(b"{", signature_start)
    if body_start < 0 or not clean_source.startswith(frozen_body, body_start):
        raise RustRpcError("frozen decision signature/body mismatch")
    body_end = body_start + len(frozen_body)
    facade = (Path(__file__).parent / "study_workers" / "rust_facade.rs").read_text()
    replacement = ("{\n" + codec_source() + facade + "\n}\n").encode()
    prefix, suffix = clean_source[:body_start], clean_source[body_end:]
    generated = prefix + replacement + suffix
    digest = lambda value: hashlib.sha256(value).hexdigest()
    generator = Path(__file__).read_bytes() + facade.encode() + frozen_body + (Path(__file__).parent / "study_workers" / "rust_worker.rs").read_bytes()
    return InstrumentedSource(generated, digest(clean_source), digest(generated), digest(prefix), digest(suffix), digest(generator), schema_sha256())


def worker_binary_source() -> bytes:
    template = (Path(__file__).parent / "study_workers" / "rust_worker.rs").read_text()
    # Public crate path for the separate binary; never edit the candidate body.
    return (codec_source().replace("crate::", "plurx_core::") + template).encode()


def compile_recipe() -> tuple[tuple[str, ...], ...]:
    """Trusted controller uses exact frozen workspace/Cargo.lock, offline cache.

    Install worker_binary_source at crates/plurx-core/src/bin/study_rpc.rs in the
    worker copy only. No manifest edit or crate/dependency substitution is used.
    Missing cache, resource failure and compiler error are distinct controller
    outcomes. This function never executes a compiler or tests.
    """
    return (("/usr/local/cargo/bin/rustc", "--version"),
            ("/usr/local/cargo/bin/cargo", "build", "--offline", "--locked", "-p", "plurx-core", "--bin", "study_rpc"))
