use serde_json::{json, Value};

fn fixture() -> Value {
    json!({
        "schema_version": 1,
        "evidence_scope": "named_runner",
        "build_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "server_build": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "scenario": "healthy",
        "started_at_unix_ms": 1_000,
        "finished_at_unix_ms": 2_000,
        "runner": {
            "hardware": "m6-pro",
            "storage_device": "nvme",
            "network_path": "lan-ethernet"
        },
        "target": {"role": "follower", "voter_count": 3},
        "sample_count_per_route": 1,
        "routes": [{
            "route": "home",
            "samples": [{
                "shell_us": 100,
                "content_us": 400,
                "settled_us": 700,
                "endpoints": [{
                    "route": "/api/v1/hubs",
                    "start_us": 120,
                    "end_us": 350,
                    "status": 200
                }],
                "failure": null
            }],
            "summary": {
                "success_count": 1,
                "failure_count": 0,
                "shell": {"sample_count": 1, "p50_us": 100, "p95_us": 100, "max_us": 100},
                "content": {"sample_count": 1, "p50_us": 400, "p95_us": 400, "max_us": 400},
                "settled": {"sample_count": 1, "p50_us": 700, "p95_us": 700, "max_us": 700}
            }
        }]
    })
}

#[test]
fn page_latency_schema_compiles_and_accepts_only_its_closed_shape() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-page-latency.schema.json"
    ))
    .expect("parse checked-in page-latency schema");
    jsonschema::draft202012::meta::validate(&schema)
        .expect("schema satisfies the Draft 2020-12 meta-schema");
    let validator = jsonschema::draft202012::new(&schema).expect("compile Draft 2020-12 schema");

    let valid = fixture();
    assert!(validator.is_valid(&valid));

    let mut unknown_field = fixture();
    unknown_field["credential"] = json!("must not fit the closed shape");
    assert!(!validator.is_valid(&unknown_field));

    let mut invalid_status = fixture();
    invalid_status["routes"][0]["samples"][0]["endpoints"][0]["status"] = json!(42);
    assert!(!validator.is_valid(&invalid_status));
}
