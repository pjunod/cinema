//! Bind a buffered response allocation to the SAME admitted finite role.
use super::super::response_memory::{FiniteResponseBuffer, ResponseBudget};
use super::*;

impl NodeResourceRuntime {
    /// Called synchronously after prepare and before any selected-path native
    /// entry. A recovering role never reconstructs allocation authority.
    pub(in crate::adaptive_resources::runtime) fn install_finite_response_budget(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        budget: Arc<ResponseBudget>,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.response_budget.is_some()
            || role.finite_vod.is_some()
            || !role.bootstrap.is_empty()
            || role.transition.is_some()
            || role.refunded
        {
            return Err(OperationError::State);
        }
        role.response_budget = Some(budget);
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) async fn allocate_finite_response_buffer(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        requested_member: Uuid,
        capacity: usize,
        deadline: std::time::Instant,
    ) -> Result<FiniteResponseBuffer, OperationError> {
        self.validate_preparation(owner)?;
        if std::time::Instant::now() >= deadline {
            return Err(OperationError::Deadline);
        }
        let slot = {
            let bundle = owner.state.lock().await;
            self.eligible(&bundle)?;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            let member = if role.bootstrap.contains_key(&requested_member) {
                requested_member
            } else {
                role.retirement
                    .as_ref()
                    .and_then(|retirement| retirement.resolve_source_member(requested_member))
                    .ok_or(OperationError::State)?
            };
            let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
            if role.refunded
                || role.transition.is_some()
                || !bootstrap.response_allocation_ready()
                || std::time::Instant::now() >= deadline
            {
                return Err(OperationError::State);
            }
            role.response_budget
                .as_ref()
                .ok_or(OperationError::State)?
                .reserve(capacity)?
        };
        if std::time::Instant::now() >= deadline {
            return Err(OperationError::Deadline);
        }
        FiniteResponseBuffer::allocate(slot, capacity)
    }
}
