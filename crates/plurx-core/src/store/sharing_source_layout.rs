//! The closed Source layout as plain SQL statements: the frozen principal
//! rebuild, the Source candidate objects and, for the adaptive variant, the
//! attempt authority's detach and principal rebind around them. Startup and
//! live coordination (`sharing_source_schema`, replicated only) and the
//! SQLite-only fixtures share these exact statements.
use super::{sharing_catalogue_source, sharing_source_sessions};

#[cfg(test)]
pub(crate) fn adaptive_layout_statements() -> Vec<String> {
    adaptive_layout_statements_for(
        super::adaptive_resource_attempts::schema::OutageFence::Standalone,
    )
}

/// The adaptive Source layout as a store reaches it in production. Source
/// activation follows membership and purpose-key setup, and the adaptive
/// rebind's ready guard proves that whole Source shape: the replicated
/// membership tables and the catalogue purpose-key table must exist. A
/// standalone SQLite fixture has neither, so it gets presence-only membership
/// tables (as `subtitle_managed_schema`'s Source82 contract fixture does;
/// they are NOT cluster admission evidence) and the exact purpose-key table,
/// then the production statements.
#[cfg(test)]
pub(crate) fn provisioned_adaptive_layout_statements() -> Vec<String> {
    let mut statements = [
        "cluster_nodes",
        "cluster_node_capabilities",
        "cluster_join_tokens",
        "cluster_node_join_staging",
        "cluster_node_removals",
        "cluster_node_removal_attempts",
        "cluster_sharing_membership_intents",
        "cluster_sharing_join_intents",
        "cluster_sharing_join_declarations",
        "cluster_sharing_membership_generation",
    ]
    .map(|name| format!("CREATE TABLE IF NOT EXISTS {name}(fixture INTEGER)"))
    .to_vec();
    statements.push(
        sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA
            .trim()
            .trim_end_matches(';')
            .to_owned(),
    );
    statements.extend(adaptive_layout_statements());
    statements
}

pub(crate) fn adaptive_layout_statements_for(
    outage: super::adaptive_resource_attempts::schema::OutageFence,
) -> Vec<String> {
    use super::adaptive_resource_attempts::schema as adaptive;
    let mut statements = adaptive::detach_session_triggers(adaptive::Layout::Legacy);
    statements.extend(layout_statements());
    statements.extend(adaptive::rebind_principal_statements_for(outage));
    statements
}

/// Frozen statement order preserves child authority rows and then seeds the
/// allocator before order-maintenance triggers can observe ordinary inserts.
pub(crate) fn layout_statements() -> Vec<String> {
    let mut statements = super::media_session_principal_rebuild_schema()
        .split("-- next statement\n")
        .map(|part| {
            part.lines()
                .skip_while(|line| line.trim().is_empty() || line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .trim_end_matches(';')
                .to_owned()
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    statements.extend(sharing_catalogue_source::candidate_item_identity_statements());
    statements.extend(sharing_catalogue_source::candidate_statements());
    statements.extend(sharing_source_sessions::candidate_statements());
    statements
}
