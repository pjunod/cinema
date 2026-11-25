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
