use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Shares one task-state vocabulary across queries and API snapshots while retaining the deployed varchar values.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    #[sea_orm(string_value = "queued")]
    Queued,
    #[sea_orm(string_value = "running")]
    Running,
    #[sea_orm(string_value = "succeeded")]
    Succeeded,
    #[sea_orm(string_value = "failed")]
    Failed,
}

impl JobStatus {
    /// Identifies persisted terminal states consistently for every subscriber and result endpoint.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}
