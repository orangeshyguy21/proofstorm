//! One lookup for all transports. Names are aliases; instance keys fence incarnations.
use super::Cells;
use crate::Error;
use proofstorm_core::{CellInstance, CellInstanceStatus};
use proofstorm_store::CellHandle;

impl Cells {
    pub fn resolve(&self, reference: &str) -> Result<CellHandle, Error> {
        Ok(self
            .store
            .resolve_cell(&self.workspace, &self.principal, reference)?)
    }

    pub fn resolve_instance(&self, reference: &str) -> Result<CellInstance, Error> {
        self.instance(&self.resolve(reference)?)
    }

    pub async fn status(&self, reference: &str) -> Result<CellInstanceStatus, Error> {
        self.runtime.status(self.resolve_instance(reference)?).await
    }
}
