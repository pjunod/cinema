//! The finite rendition's client tables have an actual reserved capacity.
//! Legacy renditions keep their existing hash maps. A finite insertion never
//! invokes a collection growth path after its table has been published.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

#[cfg(target_os = "linux")]
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteClientSlot, FiniteClientStateLease,
};
#[cfg(target_os = "linux")]
use std::sync::Arc;

/// A share of the original finite role's client partition. The wrapper keeps
/// diagnostics from treating a private core budget token as a public datum.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(super) struct ClientSlot(pub(super) Arc<FiniteClientSlot>);

#[cfg(not(target_os = "linux"))]
#[derive(Clone, Debug)]
pub(super) struct ClientSlot;

#[cfg(target_os = "linux")]
impl std::fmt::Debug for ClientSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClientSlot")
    }
}

#[cfg(target_os = "linux")]
impl ClientSlot {
    pub(super) fn validate_actual(&self, bytes: usize) -> std::io::Result<()> {
        self.0.validate_actual(bytes)
    }
}

pub(super) type ReaderTable = ClientTable<String, super::Reader>;
pub(super) type DemandTable = ClientTable<u32, super::MaterializeClock>;

pub(super) enum ClientTable<K, V> {
    Legacy(HashMap<K, V>),
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current finite/adaptive production callers are Linux-only; preserve this portable metadata/owner interface for unsupported-platform behavior and fixtures."
        )
    )]
    Finite(FixedVecMap<K, V>),
}

pub(super) struct FixedVecMap<K, V> {
    entries: Vec<(K, V)>,
    max_entries: usize,
    // Drop after the Vec allocation that this slot charges.
    #[cfg(target_os = "linux")]
    _slot: ClientSlot,
}

pub(super) enum ClientIter<'a, K, V> {
    Legacy(std::collections::hash_map::Iter<'a, K, V>),
    Finite(std::slice::Iter<'a, (K, V)>),
}

impl<'a, K, V> Iterator for ClientIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Legacy(iter) => iter.next(),
            Self::Finite(iter) => iter.next().map(|(key, value)| (key, value)),
        }
    }
}

impl<K: Eq + Hash + Ord, V> ClientTable<K, V> {
    pub(super) fn legacy() -> Self {
        Self::Legacy(HashMap::new())
    }

    /// Allocate before the finite rendition is installed, then inspect the
    /// actual capacity for its admission charge. `max_entries` is a policy
    /// limit, not an inference about allocator or hash table internals.
    #[cfg(target_os = "linux")]
    pub(super) fn finite(
        max_entries: usize,
        lease: &FiniteClientStateLease,
    ) -> std::io::Result<Self> {
        let projected = max_entries
            .checked_mul(std::mem::size_of::<(K, V)>())
            .ok_or_else(|| std::io::Error::other("finite client table size overflow"))?;
        let slot = ClientSlot(lease.reserve(projected)?);
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(max_entries)
            .map_err(std::io::Error::other)?;
        let actual = entries
            .capacity()
            .checked_mul(std::mem::size_of::<(K, V)>())
            .ok_or_else(|| std::io::Error::other("finite client table capacity overflow"))?;
        slot.validate_actual(actual)?;
        Ok(Self::Finite(FixedVecMap {
            entries,
            max_entries,
            _slot: slot,
        }))
    }

    pub(super) fn len(&self) -> usize {
        match self {
            Self::Legacy(map) => map.len(),
            Self::Finite(table) => table.entries.len(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current finite/adaptive production callers are Linux-only; preserve this portable metadata/owner interface for unsupported-platform behavior and fixtures."
        )
    )]
    pub(super) fn clear(&mut self) {
        match self {
            Self::Legacy(map) => map.clear(),
            Self::Finite(table) => table.entries.clear(),
        }
    }

    pub(super) fn has_room_for(&self, key: &K) -> bool {
        match self {
            Self::Legacy(_) => true,
            Self::Finite(table) => {
                table
                    .entries
                    .binary_search_by(|(existing, _)| existing.cmp(key))
                    .is_ok()
                    || table.entries.len() < table.max_entries
            }
        }
    }

    pub(super) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + Ord + ?Sized,
    {
        match self {
            Self::Legacy(map) => map.get(key),
            Self::Finite(table) => table
                .entries
                .binary_search_by(|(existing, _)| <K as Borrow<Q>>::borrow(existing).cmp(key))
                .ok()
                .map(|at| &table.entries[at].1),
        }
    }

    pub(super) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + Ord + ?Sized,
    {
        match self {
            Self::Legacy(map) => map.get_mut(key),
            Self::Finite(table) => {
                let at = table
                    .entries
                    .binary_search_by(|(existing, _)| <K as Borrow<Q>>::borrow(existing).cmp(key))
                    .ok()?;
                Some(&mut table.entries[at].1)
            }
        }
    }

    pub(super) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + Ord + ?Sized,
    {
        self.get(key).is_some()
    }

    pub(super) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + Ord + ?Sized,
    {
        match self {
            Self::Legacy(map) => map.remove(key),
            Self::Finite(table) => table
                .entries
                .binary_search_by(|(existing, _)| <K as Borrow<Q>>::borrow(existing).cmp(key))
                .ok()
                .map(|at| table.entries.remove(at).1),
        }
    }

    /// The finite branch moves an already constructed key/value into a slot;
    /// the failed case retains the caller's ownership for a typed refusal.
    pub(super) fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, (K, V)> {
        match self {
            Self::Legacy(map) => Ok(map.insert(key, value)),
            Self::Finite(table) => {
                let at = match table.entries.binary_search_by(|(item, _)| item.cmp(&key)) {
                    Ok(at) => {
                        // Replace the key too: its charged allocation belongs
                        // to the new reader, while the old pair drops together.
                        return Ok(Some(
                            std::mem::replace(&mut table.entries[at], (key, value)).1,
                        ));
                    }
                    Err(at) => at,
                };
                if table.entries.len() >= table.max_entries {
                    return Err((key, value));
                }
                // Capacity was reserved and validated before publication. The
                // sort invariant gives logarithmic lookups without allocating
                // a second index or growing the table.
                table.entries.insert(at, (key, value));
                Ok(None)
            }
        }
    }

    pub(super) fn iter(&self) -> ClientIter<'_, K, V> {
        match self {
            Self::Legacy(map) => ClientIter::Legacy(map.iter()),
            Self::Finite(table) => ClientIter::Finite(table.entries.iter()),
        }
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }

    pub(super) fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }

    #[cfg(test)]
    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.try_insert(key, value)
            .unwrap_or_else(|_| panic!("test inserted beyond finite client table capacity"))
    }
}

impl<V> std::ops::Index<&str> for ClientTable<String, V> {
    type Output = V;

    fn index(&self, key: &str) -> &Self::Output {
        self.get(key).expect("reader key absent")
    }
}

impl<V> std::ops::Index<&String> for ClientTable<String, V> {
    type Output = V;

    fn index(&self, key: &String) -> &Self::Output {
        self.get(key).expect("reader key absent")
    }
}

impl<V> std::ops::Index<&u32> for ClientTable<u32, V> {
    type Output = V;

    fn index(&self, key: &u32) -> &Self::Output {
        self.get(key).expect("demand key absent")
    }
}

#[cfg(target_os = "linux")]
pub(super) fn finite_tables(
    plan_entries: usize,
    lease: &FiniteClientStateLease,
) -> std::io::Result<(ReaderTable, DemandTable)> {
    use super::{MarkerPrewarmLedger, MarkerPrewarmRecord};
    use plurx_core::segplan::MAX_TIMELINE_ANNOTATIONS;

    let demand_entries = plan_entries
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("finite demand count overflow"))?;
    let demand_payload = demand_entries
        .checked_mul(std::mem::size_of::<(u32, super::MaterializeClock)>())
        .ok_or_else(|| std::io::Error::other("finite demand table overflow"))?;
    let demand_charge = FiniteClientSlot::required_bytes(demand_payload)
        .ok_or_else(|| std::io::Error::other("finite demand charge overflow"))?;
    let remaining = lease
        .limit_bytes()
        .checked_sub(demand_charge)
        .ok_or_else(|| std::io::Error::other("finite demand table exceeds client partition"))?;
    // The table reserves only entries that could have a separate ledger slot.
    // Actual session-key capacities and other aliases are charged at attach;
    // their size can reduce admission below this structural upper bound.
    let records = MAX_TIMELINE_ANNOTATIONS
        .checked_mul(std::mem::size_of::<MarkerPrewarmRecord>())
        .ok_or_else(|| std::io::Error::other("finite marker ledger overflow"))?;
    let ledger = std::mem::size_of::<std::sync::Mutex<MarkerPrewarmLedger>>()
        .checked_add(records)
        .and_then(|bytes| bytes.checked_add(2 * std::mem::size_of::<usize>()))
        .ok_or_else(|| std::io::Error::other("finite marker ledger overflow"))?;
    let table_entry = std::mem::size_of::<(String, super::Reader)>();
    let per_reader = FiniteClientSlot::required_bytes(ledger)
        .and_then(|bytes| bytes.checked_add(table_entry))
        .ok_or_else(|| std::io::Error::other("finite reader charge overflow"))?;
    let reader_entries = remaining / per_reader;
    if reader_entries == 0 {
        return Err(std::io::Error::other(
            "finite client partition has no reader slot",
        ));
    }
    // Each actual Vec is separately charged and checked before publication.
    // Allocate the demand table first, so its size cannot be crowded out by
    // a reader table when the plan is large.
    let demands = DemandTable::finite(demand_entries, lease)?;
    let readers = ReaderTable::finite(reader_entries, lease)?;
    Ok((readers, demands))
}
