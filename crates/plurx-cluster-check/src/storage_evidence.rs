#[test]
fn retained_p5_privileged_storage_evidence_matches_the_closed_schema() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-storage-physical.schema.json"
    ))
    .expect("storage physical schema JSON");
    jsonschema::draft202012::meta::validate(&schema)
        .expect("storage physical schema must be valid Draft 2020-12");
    let validator = jsonschema::draft202012::new(&schema).expect("compile storage physical schema");
    let evidence: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/evidence/p5-storage-pressure-838f20cc.json"
    ))
    .expect("retained storage physical evidence JSON");
    validator
        .validate(&evidence)
        .expect("retained storage physical evidence must match its closed schema");
}
