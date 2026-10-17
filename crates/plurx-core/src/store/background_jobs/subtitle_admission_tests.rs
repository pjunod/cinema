#[test]
fn managed_admission_matches_closed_legacy_default_and_typed_semantics() {
    let db = rusqlite::Connection::open_in_memory().expect("SQLite");
    db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT)")
        .expect("settings");
    let defaults = serde_json::json!({"enabled":false,"interval_mins":0,"command":"whisper-cli","model_path":"","language":"en"});
    let managed =
        serde_json::json!({"version":1,"mode":"managed","enabled":true,"legacy_snapshot":defaults});
    db.execute(
        "INSERT INTO settings VALUES('subtitles.transcription.managed.v1',?1)",
        [managed.to_string()],
    )
    .expect("managed");
    for (raw, expected) in [
        ("{}", true),
        (r#"{ "language":"en", "enabled":false }"#, true),
        ("null", false),
        ("", false),
        ("broken", false),
        (r#"{"enabled":null}"#, false),
        (r#"{"enabled":0}"#, false),
        (r#"{"interval_mins":null}"#, false),
        (r#"{"interval_mins":-1}"#, false),
        (r#"{"command":null}"#, false),
        (r#"{"enabled":false,"enabled":false}"#, false),
        (r#"{"unknown":false}"#, false),
        (r#"{"enabled":true}"#, false),
        (r#"{"command":"other"}"#, false),
        ("[]", false),
    ] {
        db.execute(
            "INSERT OR REPLACE INTO settings VALUES('subtitles.transcription.config',?1)",
            [raw],
        )
        .expect("legacy input");
        let allowed: bool = db
            .query_row(
                concat!("SELECT (", subtitle_managed_admission!(), ")"),
                [],
                |row| row.get(0),
            )
            .expect("closed admission predicate");
        assert_eq!(allowed, expected, "legacy document {raw:?}");
    }
    db.execute(
        "DELETE FROM settings WHERE key='subtitles.transcription.config'",
        [],
    )
    .expect("legacy absent");
    let allowed: bool = db
        .query_row(
            concat!("SELECT (", subtitle_managed_admission!(), ")"),
            [],
            |row| row.get(0),
        )
        .expect("absent predicate");
    assert!(
        !allowed,
        "absence is not explicit defaults after managed activation"
    );
}
