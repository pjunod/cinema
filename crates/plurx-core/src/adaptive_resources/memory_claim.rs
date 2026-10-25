//! Persisted producer-peak policy in this ledger's single node-RAM domain.
//! Numeric vectors retain known bounded allocations; this claim records an
//! unknown additional producer peak without encoding that peak as zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MemoryClaim {
    KnownBounded {
        active_producer: bool,
    },
    UnknownProducerPeak,
    /// Prepared heap-only M role paired atomically with the fixed probe P.
    PlanningMetadata,
}
impl Default for MemoryClaim {
    fn default() -> Self {
        Self::KnownBounded {
            active_producer: true,
        }
    }
}
impl MemoryClaim {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn is_active_producer(self) -> bool {
        !matches!(
            self,
            Self::KnownBounded {
                active_producer: false
            } | Self::PlanningMetadata
        )
    }
    pub(crate) fn sql_key(self) -> &'static str {
        match self {
            Self::KnownBounded {
                active_producer: true,
            } => "bounded_active",
            Self::KnownBounded {
                active_producer: false,
            } => "bounded_resident",
            Self::UnknownProducerPeak => "unknown_producer",
            Self::PlanningMetadata => "planning_metadata",
        }
    }
    pub(crate) fn from_sql_key(key: &str) -> Option<Self> {
        match key {
            "bounded_active" => Some(Self::default()),
            "bounded_resident" => Some(Self::KnownBounded {
                active_producer: false,
            }),
            "unknown_producer" => Some(Self::UnknownProducerPeak),
            "planning_metadata" => Some(Self::PlanningMetadata),
            _ => None,
        }
    }
}
