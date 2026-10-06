//! Bounded durable outer-ingress obligations. These values convey metadata
//! lineage only; a closed marker is written by the daemon after an exact
//! authenticated receipt for actual accepted-driver closure.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub const INGRESS_CONNECTIONS_PER_SESSION: usize = 32;
pub const INGRESS_CONNECTIONS_PER_NODE: usize = 8;
pub const INGRESS_NODES_MAX: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressRegistration {
    pub node_id: String,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub boot_id: Uuid,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub connection_id: Uuid,
    pub driver_sequence: u64,
    pub registration_sequence: u64,
    pub closed_confirmation: Option<String>,
}
impl IngressRegistration {
    pub fn valid(&self) -> bool {
        !self.node_id.is_empty()
            && self.node_id.len() <= 256
            && !self.node_id.chars().any(char::is_control)
            && [self.boot_id, self.connection_id]
                .iter()
                .all(|id| !id.is_nil() && id.get_version_num() == 4)
            && (1..=9_007_199_254_740_991).contains(&self.driver_sequence)
            && (1..=9_007_199_254_740_991).contains(&self.registration_sequence)
            && self
                .closed_confirmation
                .as_ref()
                .is_none_or(|value| crate::sharing::is_hash(value))
    }
    pub fn same_driver(&self, other: &Self) -> bool {
        self.node_id == other.node_id
            && self.boot_id == other.boot_id
            && self.connection_id == other.connection_id
            && self.driver_sequence == other.driver_sequence
            && self.registration_sequence == other.registration_sequence
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Highwater {
    node_id: String,
    boot_id: Uuid,
    sequence: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressCustodyState {
    version: u8,
    pub sealed: bool,
    slots: Vec<IngressRegistration>,
    highwater: Vec<Highwater>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustodyMutation {
    Applied,
    Replay,
    Refused,
}
impl Default for IngressCustodyState {
    fn default() -> Self {
        Self {
            version: 1,
            sealed: false,
            slots: Vec::new(),
            highwater: Vec::new(),
        }
    }
}
impl IngressCustodyState {
    pub fn decode(json: &str) -> Result<Self, crate::error::StoreError> {
        if json.len() > 65536 {
            return Err(crate::sharing::invalid());
        }
        let state: Self = serde_json::from_str(json).map_err(|_| crate::sharing::invalid())?;
        if state.version != 1
            || state.slots.len() > INGRESS_CONNECTIONS_PER_SESSION
            || state.highwater.len() > INGRESS_NODES_MAX
            || state.slots.iter().any(|slot| !slot.valid())
            || state.highwater.iter().any(|mark| {
                mark.node_id.is_empty()
                    || mark.node_id.len() > 256
                    || mark.node_id.chars().any(char::is_control)
                    || mark.boot_id.is_nil()
                    || mark.boot_id.get_version_num() != 4
                    || !(1..=9_007_199_254_740_991).contains(&mark.sequence)
            })
            || state.slots.iter().any(|slot| {
                // A differing boot is retained prior-process custody/history,
                // never a current boot or a restart closure proof. Closed
                // prior-boot slots still carry the original exact ack.
                !state.highwater.iter().any(|mark| {
                    mark.node_id == slot.node_id
                        && (mark.boot_id != slot.boot_id
                            || slot.registration_sequence <= mark.sequence)
                }) || state
                    .slots
                    .iter()
                    .filter(|other| {
                        other.closed_confirmation.is_none() && other.node_id == slot.node_id
                    })
                    .count()
                    > INGRESS_CONNECTIONS_PER_NODE
            })
            || state.slots.iter().enumerate().any(|(index, slot)| {
                state.slots[..index]
                    .iter()
                    .any(|other| slot.same_driver(other))
            })
            || state.highwater.iter().enumerate().any(|(index, mark)| {
                state.highwater[..index]
                    .iter()
                    .any(|other| mark.node_id == other.node_id)
            })
        {
            return Err(crate::sharing::invalid());
        }
        Ok(state)
    }
    pub fn encode(&self) -> Result<String, crate::error::StoreError> {
        let json = serde_json::to_string(self).map_err(|_| crate::sharing::invalid())?;
        if json.len() > 65536 {
            return Err(crate::sharing::invalid());
        }
        Ok(json)
    }
    /// The caller verifies current member/boot proof in the same transaction.
    /// Exact active replay precedes the highwater check: a lost register answer
    /// must not lose custody merely because a later connection was registered.
    pub fn register(&mut self, registration: IngressRegistration) -> CustodyMutation {
        if !registration.valid() || registration.closed_confirmation.is_some() {
            return CustodyMutation::Refused;
        }
        if let Some(slot) = self
            .slots
            .iter()
            .find(|slot| slot.same_driver(&registration))
        {
            return if slot.closed_confirmation.is_none() {
                CustodyMutation::Replay
            } else {
                CustodyMutation::Refused
            };
        }
        if self.sealed
            || self.highwater.iter().any(|mark| {
                mark.node_id == registration.node_id
                    && mark.boot_id == registration.boot_id
                    && registration.registration_sequence <= mark.sequence
            })
        {
            return CustodyMutation::Refused;
        }
        let open = self
            .slots
            .iter()
            .filter(|slot| slot.closed_confirmation.is_none())
            .count();
        let node_open = self
            .slots
            .iter()
            .filter(|slot| {
                slot.closed_confirmation.is_none() && slot.node_id == registration.node_id
            })
            .count();
        if open >= INGRESS_CONNECTIONS_PER_SESSION || node_open >= INGRESS_CONNECTIONS_PER_NODE {
            return CustodyMutation::Refused;
        }
        if let Some(mark) = self
            .highwater
            .iter_mut()
            .find(|mark| mark.node_id == registration.node_id)
        {
            mark.boot_id = registration.boot_id;
            mark.sequence = registration.registration_sequence;
        } else {
            if self.highwater.len() >= INGRESS_NODES_MAX {
                return CustodyMutation::Refused;
            }
            self.highwater.push(Highwater {
                node_id: registration.node_id.clone(),
                boot_id: registration.boot_id,
                sequence: registration.registration_sequence,
            });
        }
        self.slots.retain(|slot| slot.closed_confirmation.is_none());
        self.slots.push(registration);
        CustodyMutation::Applied
    }
    pub fn seal(&mut self) -> CustodyMutation {
        if self.sealed {
            CustodyMutation::Replay
        } else {
            self.sealed = true;
            CustodyMutation::Applied
        }
    }
    /// Called only with the exact authenticated actual-closure receipt. This
    /// function checks identity and replay; it never interprets absence/expiry.
    pub fn acknowledge(
        &mut self,
        registration: &IngressRegistration,
        confirmation: &str,
    ) -> CustodyMutation {
        if !crate::sharing::is_hash(confirmation) {
            return CustodyMutation::Refused;
        }
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.same_driver(registration))
        else {
            return CustodyMutation::Refused;
        };
        match &slot.closed_confirmation {
            Some(existing) if existing == confirmation => CustodyMutation::Replay,
            Some(_) => CustodyMutation::Refused,
            None => {
                slot.closed_confirmation = Some(confirmation.to_owned());
                CustodyMutation::Applied
            }
        }
    }
    pub fn contains_driver(&self, registration: &IngressRegistration) -> bool {
        self.slots.iter().any(|slot| slot.same_driver(registration))
    }
    pub fn open(&self) -> impl Iterator<Item = &IngressRegistration> {
        self.slots
            .iter()
            .filter(|slot| slot.closed_confirmation.is_none())
    }
    pub fn settled(&self) -> bool {
        self.sealed && self.open().next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registration(sequence: u64) -> IngressRegistration {
        IngressRegistration {
            node_id: "ingress".into(),
            boot_id: Uuid::from_u128(0x11111111111141118111111111111111),
            connection_id: Uuid::new_v4(),
            driver_sequence: sequence,
            registration_sequence: sequence,
            closed_confirmation: None,
        }
    }
    #[test]
    fn sharing_ingress_lost_register_answer_and_closed_slot_reuse_never_reopen_old_writer() {
        let mut state = IngressCustodyState::default();
        let first = registration(1);
        assert_eq!(state.register(first.clone()), CustodyMutation::Applied);
        assert_eq!(state.register(registration(2)), CustodyMutation::Applied);
        assert_eq!(state.register(first.clone()), CustodyMutation::Replay);
        assert_eq!(
            state.acknowledge(&first, &"a".repeat(64)),
            CustodyMutation::Applied
        );
        assert_eq!(
            state.acknowledge(&first, &"a".repeat(64)),
            CustodyMutation::Replay
        );
        assert_eq!(state.register(registration(3)), CustodyMutation::Applied);
        assert_eq!(state.register(first), CustodyMutation::Refused);
    }
    #[test]
    fn sharing_ingress_owner_crash_snapshot_and_unreachable_driver_refuse_settlement() {
        let mut state = IngressCustodyState::default();
        let first = registration(1);
        state.register(first.clone());
        state.seal();
        let mut restored =
            IngressCustodyState::decode(&state.encode().expect("snapshot")).expect("restore");
        assert!(
            !restored.settled(),
            "owner crash never erases outer writer custody"
        );
        assert_eq!(restored.register(registration(2)), CustodyMutation::Refused);
        assert_eq!(
            restored.acknowledge(&registration(3), &"a".repeat(64)),
            CustodyMutation::Refused
        );
        assert!(
            !restored.settled(),
            "missing or unreachable ingress provides no physical closure"
        );
        assert_eq!(
            restored.acknowledge(&first, &"a".repeat(64)),
            CustodyMutation::Applied
        );
        assert!(restored.settled());
    }
    #[test]
    fn sharing_ingress_caps_count_open_connections_and_completed_requests_reclaim_slots() {
        let mut state = IngressCustodyState::default();
        for sequence in 1..=128 {
            let current = registration(sequence);
            assert_eq!(state.register(current.clone()), CustodyMutation::Applied);
            assert_eq!(
                state.acknowledge(&current, &"b".repeat(64)),
                CustodyMutation::Applied
            );
        }
        let mut held = Vec::new();
        for sequence in 129..=136 {
            let current = registration(sequence);
            assert_eq!(state.register(current.clone()), CustodyMutation::Applied);
            held.push(current);
        }
        assert_eq!(state.register(registration(137)), CustodyMutation::Refused);
        state.acknowledge(&held[0], &"b".repeat(64));
        assert_eq!(state.register(registration(138)), CustodyMutation::Applied);
    }
}
