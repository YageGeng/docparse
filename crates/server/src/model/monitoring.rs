//! Wire contracts for local snapshots and bounded historical queries.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

/// Fixed low-cardinality metric values, with histograms represented by their sum/count series.
#[derive(Serialize, ToSchema)]
pub struct Sample {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

/// Local observations include collection time so clients can detect stale snapshots.
#[derive(Serialize, ToSchema, typed_builder::TypedBuilder)]
pub struct Snapshot {
    pub collected_at: i64,
    pub history_available: bool,
    pub samples: Vec<Sample>,
    pub process: Process,
}

/// Identifies a local recorder without adding unbounded process IDs to metric labels.
#[derive(Clone, Serialize, ToSchema)]
pub struct Process {
    pub id: String,
    pub role: &'static str,
}

/// Allowlisting bounds both query cost and metric exposure; arbitrary PromQL is never forwarded.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Chart {
    QueueWait,
    AdmissionWait,
    PagePressure,
    Oldest,
    Turnaround,
    Backlog,
    Queue,
    Blocked,
    Throughput,
    Wait,
    Parse,
    Inference,
    Workers,
    Pages,
}
/// Historical requests have a fixed point budget and a maximum seven-day window.
#[derive(Deserialize, utoipa::IntoParams)]
pub struct HistoryQuery {
    pub chart: Chart,
    pub seconds: u32,
}
