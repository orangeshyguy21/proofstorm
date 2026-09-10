//! One lookup for all transports. Names are aliases; instance keys fence incarnations.
use super::Labs;
use crate::Error;
use proofstorm_core::{LabInstance, LabInstanceStatus};
use proofstorm_store::LabHandle;

impl Labs {
    pub fn resolve(&self, reference: &str) -> Result<LabHandle, Error> {
        Ok(self
            .store
            .resolve_lab(&self.workspace, &self.principal, reference)?)
    }

    pub fn resolve_instance(&self, reference: &str) -> Result<LabInstance, Error> {
        self.instance(&self.resolve(reference)?)
    }

    pub async fn status(&self, reference: &str) -> Result<LabInstanceStatus, Error> {
        self.runtime.status(self.resolve_instance(reference)?).await
    }
}
