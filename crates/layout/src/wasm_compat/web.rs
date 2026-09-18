//! Browser model initialization policy, metadata, and Worker-local CPU execution.
use crate::{LayoutError, ModelMetadataSchema};
use ort::session::Session;
use std::collections::BTreeMap;

use crate::PpDocLayoutV3Engine;
use docparse_config::ValidatedConfig;
use std::sync::Arc;

impl PpDocLayoutV3Engine {
    /// Rejects an implicit filesystem model source in a browser.
    pub async fn from_config(
        _config: Arc<ValidatedConfig>,
    ) -> Result<Self, LayoutError> {
        tracing::error!("browser model creation requires explicit artifacts");
        Err(LayoutError::ModelArtifactsRequired)
    }
}

/// Represents descriptive metadata explicitly as unavailable on the Web backend.
pub fn model_metadata(
    _session: &Session,
) -> Result<ModelMetadataSchema, LayoutError> {
    Ok(ModelMetadataSchema::builder()
        .custom(BTreeMap::new())
        .build())
}
