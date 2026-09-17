//! Local catalogue rules; subject matching requires no external model service.
use plurx_core::{
    channel_subjects::{Metadata, SubjectDecision},
    metadata::classification::{local_decision, VERSION},
};
use std::collections::BTreeMap;
pub struct LocalSearch;
impl LocalSearch {
    pub fn configured() -> Result<Self, String> {
        Ok(Self)
    }
    pub async fn profile(&self) -> Result<String, String> {
        Ok(format!("local:{VERSION}"))
    }
    pub async fn classify(
        &self,
        subject: &str,
        records: &[(String, Metadata)],
    ) -> Result<BTreeMap<String, SubjectDecision>, String> {
        Ok(records
            .iter()
            .map(|(id, m)| (id.clone(), local_decision(subject, m)))
            .collect())
    }
}
