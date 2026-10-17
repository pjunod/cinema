use super::*;
use crate::store::hiqlite::disconnected_test_client;

#[tokio::test]
async fn shared_read_helpers_reject_misordered_placeholders() {
    let client = disconnected_test_client();
    for error in [
        scalar(&client, "SELECT $2 AS value WHERE $1 = 1")
            .await
            .expect_err("scalar helper must validate SQL"),
        scalar_with(
            &client,
            "SELECT $2 AS value WHERE $1 = 1".to_owned(),
            params!(1, 2),
        )
        .await
        .expect_err("parameterized scalar helper must validate SQL"),
        pairs(&client, "SELECT $2 AS label, $1 AS count")
            .await
            .expect_err("pairs helper must validate SQL"),
    ] {
        assert!(error.to_string().contains("expected $1, found $2"));
    }
}
