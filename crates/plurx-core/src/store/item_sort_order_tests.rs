use super::item_sort_order_by;
use crate::domain::ItemSort;

/// Every library sort ends in a unique key.
///
/// This is a text assertion on the clause, and it is deliberately not the
/// fixture replay in `tests/store_contract.rs`. With movies alone that
/// replay did not detect the tie-break's removal: SQLite returned the tied
/// rows in rowid order, which is the order the fixture expects. It now
/// does — the fixture's photo/movie pair ties on every visible key, and
/// the grid query reads through `idx_items_library_kind`, which yields
/// them in kind order — but that is still an observation of today's query
/// plan. What SQLite does with tied rows is not promised by anything; it
/// can change with the plan, the schema, an added index or the backend,
/// and when it changes the symptom is a client paging by `offset` that
/// reads two adjacent pages of two different orderings: an item on the
/// seam shown twice and its neighbour never shown at all. A property
/// nothing guarantees cannot be pinned by observing that it currently
/// holds; it has to be pinned by requiring the clause that makes it true.
///
/// `Added` ends in `id DESC` and the other four in `id ASC`. `Added` is
/// not made symmetric on purpose: it already had a tie-break, and flipping
/// it would reorder equal-`added_at` rows that viewers see today for
/// nothing.
#[test]
fn every_sort_ends_in_a_unique_key() {
    for sort in [
        ItemSort::Title,
        ItemSort::Added,
        ItemSort::Year,
        ItemSort::Resolution,
        ItemSort::Recorded,
    ] {
        let clause = item_sort_order_by(sort);
        let last = clause
            .rsplit(',')
            .next()
            .expect("a non-empty ORDER BY")
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(
            last.first().copied(),
            Some("id"),
            "{sort:?} ends in {clause:?}, which has no unique final key"
        );
        assert!(
            matches!(last.get(1).copied(), Some("ASC") | Some("DESC")),
            "{sort:?} ends in {clause:?}, whose final key has no explicit direction"
        );
    }
    assert!(item_sort_order_by(ItemSort::Added).ends_with("id DESC"));
}

/// The `resolution` sort ranks by height exactly the kinds whose DTO
/// carries a `resolution`, and every other kind at -1.
///
/// The clause and `ItemKind::carries_resolution` are two spellings of one
/// rule — SQL cannot call the Rust predicate — so this reads the kind list
/// out of the clause and compares it with the predicate for every kind. A
/// kind added to one and not the other is a row the server ranks by a key
/// the client never receives.
#[test]
fn resolution_ranks_exactly_the_kinds_that_carry_one() {
    use crate::domain::ItemKind;

    let clause = item_sort_order_by(ItemSort::Resolution);
    let list = clause
        .strip_prefix("CASE WHEN kind IN (")
        .and_then(|rest| rest.split_once(')'))
        .map(|(list, _)| list)
        .unwrap_or_else(|| panic!("{clause:?} does not rank by kind first"));
    let ranked: std::collections::BTreeSet<&str> = list
        .split(',')
        .map(|kind| kind.trim().trim_matches('\''))
        .collect();
    for kind in [
        ItemKind::Movie,
        ItemKind::Show,
        ItemKind::Season,
        ItemKind::Episode,
        ItemKind::Book,
        ItemKind::Audiobook,
        ItemKind::Folder,
        ItemKind::Video,
        ItemKind::Photo,
    ] {
        assert_eq!(
            ranked.contains(kind.as_str()),
            kind.carries_resolution(),
            "{kind:?}: the resolution sort and the DTO disagree about whether it has a resolution"
        );
    }
    assert!(clause.contains("ELSE -1 END DESC"), "{clause:?}");
}

/// The clause is one function, and every backend's page and count use it.
///
/// Three copies of this `match` is how the SQLite and Hiqlite stores would
/// drift apart, and a merge order that differs between backends is a
/// defect no single-backend test can see — `for_each_backend` runs the
/// Hiqlite voters only under `hiqlite-contract-tests`, so on an ordinary
/// run the fixture replay never compares them at all.
#[test]
fn the_stores_do_not_carry_their_own_copies_of_the_order() {
    for source in [
        include_str!("sqlite/media.rs"),
        include_str!("hiqlite_media.rs"),
    ] {
        assert!(
            !source.contains("ItemSort::Title =>"),
            "a media store spells the library ORDER BY itself again"
        );
        assert!(source.contains("item_sort_order_by(sort)"));
    }
}
