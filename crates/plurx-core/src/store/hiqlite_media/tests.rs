use super::*;

#[test]
fn fts_expression_quotes_tokens_and_prefixes_only_the_last() {
    assert_eq!(
        fts_query("The Matrix"),
        Some("(\"the\") AND (\"matrix\"*)".to_owned())
    );
    assert_eq!(fts_query("..."), None);
}

#[test]
fn in_transaction_budget_refusal_is_not_mislabeled_as_a_root_change() {
    assert_eq!(
        reconcile_refusal(Some("same-root"), "same-root", 2, 1),
        Some(ReconcileOutcome::RefusedPrune {
            requested: 2,
            limit: 1,
        })
    );
}

#[test]
fn replicated_id_sets_are_bound_as_json_instead_of_inlined_sql() {
    let source = include_str!("../hiqlite_media.rs");
    assert!(!source.contains(&["fn id", "_list"].concat()));
    assert!(!source.contains(&["IN ({", "list})"].concat()));
    assert_eq!(ids_json(&[3, 5, 8]).expect("JSON ids"), "[3,5,8]");
}
