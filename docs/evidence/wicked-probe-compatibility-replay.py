#!/usr/bin/env python3
"""Replay the 2026-09-17 E-AC-3 refusal against both comparators.

`deployed` is the source-probe comparator exactly as it stood on the node that
refused the session, extracted from a pinned revision and checked by digest.
`shipped` is the comparator in this working tree. The synthetic suite proves
that the deployed one refuses the legacy/modern E-AC-3 pair, that removing only
`/streams/2/profile` makes it accept, and that the shipped one admits that one
omission without loosening anything else.

With `--real-pair STORED HELD` it runs both comparators over two real FFprobe
documents instead. That is how the fixture gap is closed: the synthetic suite
proves the rule, the real pair proves it on the movie. Neither probe is stored
here — private media metadata does not belong in the repository.

Needs the pinned toolchain and a warm Cargo registry (it builds `--offline`,
so `serde`/`serde_json` must already be cached). No network, no server access.
"""

from pathlib import Path
import argparse
import hashlib
import json
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[2]
# The revision serving the refusal, and the digest of the comparator slice as
# it stood there. Both are the incident's evidence and must not be re-pointed
# at a later commit: the whole question is what the deployed code did.
DEPLOYED_REVISION = "c9e4edf451e12247a7aa4188903e5ba36888e7e9"
DEPLOYED_DIGEST = "d07b5601eb330904573fbcee28a2cd3e597f401937bc7b5611a7c29ba894ea6c"
SLICE_START = "fn normalized_probe_document("
SLICE_END = "/// Which pacing flags"
SHIPPED_SOURCE = ROOT / "crates/plurxd/src/ffmpeg.rs"

PRELUDE = "#![allow(dead_code, unused_imports, clippy::all)]\n"

DRIVER = r'''
fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [stored_path, held_path] = arguments.as_slice() else {
        eprintln!("usage: wicked-probe-replay <stored probe.json> <held probe.json>");
        std::process::exit(2);
    };
    let stored = std::fs::read_to_string(stored_path).expect("stored probe");
    let held = std::fs::read_to_string(held_path).expect("held probe");
    let deployed = deployed::probes_describe_same_input(&stored, &held).expect("deployed compare");
    let shipped = shipped::compare_probe_documents(&stored, &held).expect("shipped compare");
    println!(
        "{}",
        serde_json::json!({
            "deployed_admits": deployed,
            "shipped_admits": shipped.same,
            "shipped_differences": if shipped.same { String::new() } else { shipped.rendered_differences() },
            "shipped_truncated": shipped.truncated,
        })
    );
}
'''

TESTS = r'''
#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    fn legacy() -> Value {
        json!({
            "format": {"duration": "60.000000"},
            "chapters": [],
            "streams": [
                {"index":0,"codec_type":"video","codec_name":"hevc",
                 "width":3840,"height":2160,"r_frame_rate":"24/1"},
                {"index":1,"codec_type":"audio","codec_name":"truehd","channels":8},
                {"index":2,"codec_type":"audio","codec_name":"eac3","channels":6,
                 "sample_rate":"48000","channel_layout":"5.1(side)"}
            ]
        })
    }

    fn modern() -> Value {
        let mut v = legacy();
        v["streams"][1]["profile"] = json!("Dolby TrueHD + Dolby Atmos");
        v["streams"][2]["profile"] = json!("Dolby Digital Plus + Dolby Atmos");
        v
    }

    fn compare(a: &Value, b: &Value, shipped: bool) -> bool {
        if shipped {
            super::shipped::compare_probe_documents(&a.to_string(), &b.to_string())
                .unwrap()
                .same
        } else {
            super::deployed::probes_describe_same_input(&a.to_string(), &b.to_string()).unwrap()
        }
    }

    #[test]
    fn deployed_refuses_eac3_omission_in_both_directions() {
        assert!(!compare(&legacy(), &modern(), false));
        assert!(!compare(&modern(), &legacy(), false));
    }

    #[test]
    fn eac3_profile_is_the_only_remaining_trigger() {
        let mut v = modern();
        v["streams"][2].as_object_mut().unwrap().remove("profile");
        assert!(compare(&legacy(), &v, false));
        assert!(compare(&v, &legacy(), false));
    }

    #[test]
    fn shipped_rule_accepts_only_the_measured_omission() {
        assert!(compare(&legacy(), &modern(), true));
        assert!(compare(&modern(), &legacy(), true));
        for profile in [json!(null), json!(42), json!(""), json!("unknown"),
                        json!("Dolby Digital Plus"), json!("Dolby TrueHD + Dolby Atmos")] {
            let mut v = modern();
            v["streams"][2]["profile"] = profile;
            assert!(!compare(&legacy(), &v, true));
            assert!(!compare(&v, &legacy(), true));
        }
    }

    #[test]
    fn shipped_rule_preserves_material_changes() {
        for (pointer, value) in [
            ("/streams/0/width", json!(1920)),
            ("/streams/0/r_frame_rate", json!("25/1")),
            ("/streams/2/index", json!(3)),
            ("/streams/2/codec_name", json!("ac3")),
            ("/streams/2/codec_type", json!("video")),
            ("/streams/2/channels", json!(2)),
            ("/streams/2/sample_rate", json!("44100")),
            ("/streams/2/channel_layout", json!("stereo")),
            ("/format/duration", json!("61.000000")),
            ("/chapters", json!([{"id":0,"start":0,"end":1000}]))
        ] {
            let mut v = modern();
            *v.pointer_mut(pointer).unwrap() = value;
            assert!(!compare(&legacy(), &v, true), "{pointer}");
            assert!(!compare(&v, &legacy(), true), "{pointer}");
        }
        let mut v = modern();
        v["streams"].as_array_mut().unwrap().pop();
        assert!(!compare(&legacy(), &v, true));
    }

    #[test]
    fn two_reported_profiles_still_must_agree() {
        let mut v = modern();
        v["streams"][2]["profile"] = json!("Dolby Digital Plus");
        assert!(!compare(&modern(), &v, true));
        assert!(!compare(&v, &modern(), true));
        assert!(compare(&modern(), &modern(), true));
    }

    #[test]
    fn exception_does_not_leak_to_other_codecs_or_missing_identity() {
        for codec in ["ac3", "aac", "hevc"] {
            let mut a = legacy();
            let mut b = modern();
            a["streams"][2]["codec_name"] = json!(codec);
            b["streams"][2]["codec_name"] = json!(codec);
            assert!(!compare(&a, &b, true));
        }
        for index in [None, Some(json!(null)), Some(json!(-1)), Some(json!("2"))] {
            let mut a = legacy();
            let mut b = modern();
            for v in [&mut a, &mut b] {
                let st = v["streams"][2].as_object_mut().unwrap();
                st.remove("index");
                if let Some(ref index) = index { st.insert("index".into(), index.clone()); }
            }
            assert!(!compare(&a, &b, true));
        }
    }

    /// The refusal an operator reads has to name a field and nothing private.
    #[test]
    fn shipped_refusal_names_the_field_without_leaking_text() {
        let mut stored = legacy();
        stored["format"]["tags"] = json!({"title": "a private title"});
        let mut held = modern();
        held["format"]["tags"] = json!({"title": "another private title"});
        held["streams"][0]["width"] = json!(1920);
        let comparison =
            super::shipped::compare_probe_documents(&stored.to_string(), &held.to_string())
                .unwrap();
        assert!(!comparison.same);
        let rendered = comparison.rendered_differences();
        assert!(rendered.contains("/streams/0/width value"), "{rendered}");
        assert!(rendered.contains("/format/tags/<field>"), "{rendered}");
        assert!(!rendered.contains("private"), "{rendered}");
        assert!(!rendered.contains("title"), "{rendered}");
    }
}
'''


def slice_of(text: str) -> str:
    return text[text.index(SLICE_START):text.index(SLICE_END)]


def project(root: Path, deployed: str, shipped: str) -> Path:
    (root / "src").mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(
        '[package]\nname="wicked-probe-replay"\nversion="0.0.0"\nedition="2024"\n'
        '[dependencies]\nserde_json="1"\nserde={version="1",features=["derive"]}\n'
    )
    (root / "src/main.rs").write_text(
        PRELUDE
        + "mod deployed {\n" + deployed + "}\n"
        + "mod shipped {\n" + shipped + "}\n"
        + DRIVER + TESTS
    )
    return root / "Cargo.toml"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--real-pair", nargs=2, metavar=("STORED", "HELD"),
        help="two FFprobe JSON documents to compare instead of the synthetic suite",
    )
    options = parser.parse_args()

    compiler = subprocess.check_output(
        ["rustup", "run", "1.97.1", "rustc", "--version"], text=True
    ).strip()
    assert compiler.startswith("rustc 1.97.1 "), compiler
    deployed = slice_of(subprocess.check_output(
        ["git", "show", f"{DEPLOYED_REVISION}:crates/plurxd/src/ffmpeg.rs"],
        cwd=ROOT, text=True,
    ))
    digest = hashlib.sha256(deployed.encode()).hexdigest()
    assert digest == DEPLOYED_DIGEST, digest
    shipped = slice_of(SHIPPED_SOURCE.read_text())
    print(f"Compiler: {compiler}")
    print(f"Deployed: {DEPLOYED_REVISION} (comparator SHA-256 {DEPLOYED_DIGEST})")
    print(f"Shipped:  {SHIPPED_SOURCE.relative_to(ROOT)} in this working tree", flush=True)

    with tempfile.TemporaryDirectory(prefix="wicked-probe-replay-") as tmp:
        manifest = project(Path(tmp), deployed, shipped)
        if not options.real_pair:
            subprocess.run(
                ["rustup", "run", "1.97.1", "cargo", "test", "--offline", "--quiet",
                 "--manifest-path", str(manifest)], check=True,
            )
            return
        stored, held = (str(Path(p).resolve()) for p in options.real_pair)
        out = subprocess.check_output(
            ["rustup", "run", "1.97.1", "cargo", "run", "--offline", "--quiet",
             "--manifest-path", str(manifest), "--", stored, held], text=True,
        )
    verdict = json.loads(out)
    print(json.dumps(verdict, indent=2))
    assert verdict["deployed_admits"] is False, "the deployed comparator did not refuse this pair"
    assert verdict["shipped_admits"] is True, "the shipped comparator did not admit this pair"


if __name__ == "__main__":
    main()
