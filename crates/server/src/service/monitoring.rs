//! Recorder installation, background collection and historical query policy.
use crate::{
    code::ApiCode,
    error::{ApiResult, RequestSnafu},
    model::monitoring::{Chart, HistoryQuery, Process, Sample, Snapshot},
};
use docparse_database::{
    query::parse_job::ParseJobQuery as Jobs, seaorm::DatabaseConnection,
};
use metrics_exporter_prometheus::{
    Matcher, PrometheusBuilder, PrometheusHandle,
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// One process owns a recorder and a bounded, server-configured historical query client.
#[derive(typed_builder::TypedBuilder)]
pub struct Monitoring {
    handle: PrometheusHandle,
    client: reqwest::Client,
    #[builder(default)]
    upstream: Option<reqwest::Url>,
    process: Process,
}

impl Monitoring {
    /// Installs before model creation so capacity and model initialization are never missed.
    pub fn install(
        upstream: Option<&str>,
        role: &'static str,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let upstream = upstream.map(reqwest::Url::parse).transpose()?;
        if upstream.as_ref().is_some_and(|url| {
            !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
        }) {
            return Err("monitoring.prometheus_url must be an HTTP(S) base URL without credentials, query, or fragment".into());
        }
        let recorder = PrometheusBuilder::new()
            .add_global_label("service", "docparse")
            .set_buckets(&[
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
                10.0, 30.0, 60.0, 120.0, 300.0, 900.0, 3600.0,
            ])?
            .set_buckets_for_metric(
                Matcher::Full("docparse_onnx_batch_items".into()),
                &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0],
            )?
            .build_recorder();
        let handle = recorder.handle();
        metrics::set_global_recorder(recorder)?;
        metrics::gauge!("docparse_process_start_time_seconds")
            .set(chrono::Utc::now().timestamp() as f64);
        metrics::gauge!("docparse_database_collection_success").set(0.0);
        for name in [
            "docparse_jobs_submitted_total",
            "docparse_pages_parsed_total",
        ] {
            metrics::counter!(name).increment(0);
        }
        for outcome in ["succeeded", "failed"] {
            metrics::counter!("docparse_jobs_completed_total", "outcome" => outcome).increment(0);
        }
        for name in [
            "docparse_page_slots_used",
            "docparse_pdfium_documents_active",
        ] {
            metrics::gauge!(name).set(0.0);
        }
        metrics::gauge!("docparse_build_info", "version" => env!("CARGO_PKG_VERSION")).set(1.0);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Arc::new(
            Self::builder()
                .handle(handle)
                .client(client)
                .upstream(upstream)
                .process(Process {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                })
                .build(),
        ))
    }

    /// Periodically refreshes shared database gauges; failures retain values and mark them stale.
    pub fn collect(
        self: &Arc<Self>,
        db: DatabaseConnection,
        shutdown: CancellationToken,
    ) {
        let monitor = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(
                tokio::time::MissedTickBehavior::Skip,
            );
            loop {
                tokio::select! { _ = shutdown.cancelled() => break, _ = interval.tick() => {} }
                let result = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    result = tokio::time::timeout(Duration::from_secs(4), Jobs::backlog(&db)) => result,
                };
                match result {
                    Ok(Ok(backlog)) => {
                        let now = chrono::Utc::now();
                        for (state, count, oldest) in backlog {
                            metrics::gauge!("docparse_jobs", "state" => state, "scope" => "database").set(count as f64);
                            metrics::gauge!("docparse_job_oldest_age_seconds", "state" => state, "scope" => "database")
                                .set(oldest.map_or(0.0, |time| (now.timestamp_millis() - time.timestamp_millis()).max(0) as f64 / 1000.0));
                        }
                        metrics::gauge!(
                            "docparse_database_collection_timestamp_seconds"
                        )
                        .set(now.timestamp() as f64);
                        metrics::gauge!("docparse_database_collection_success")
                            .set(1.0);
                    }
                    _ => {
                        metrics::gauge!("docparse_database_collection_success")
                            .set(0.0);
                        metrics::counter!(
                            "docparse_database_collection_errors_total"
                        )
                        .increment(1);
                    }
                }
                monitor.handle.run_upkeep();
            }
        });
    }
}

impl Monitoring {
    /// Renders the shared recorder without querying the database on each scrape.
    pub fn render(&self) -> String {
        self.handle.render()
    }

    /// Projects finite recorder samples into the same local snapshot contract for every caller.
    pub fn snapshot(&self) -> ApiResult<Snapshot> {
        let scrape = prometheus_parse::Scrape::parse(
            self.handle.render().lines().map(|line| Ok(line.to_owned())),
        )
        .map_err(|_error| {
            RequestSnafu {
                stage: "metrics-snapshot",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            }
            .build()
        })?;
        let samples = scrape
            .samples
            .into_iter()
            .filter_map(|sample| {
                let value = match sample.value {
                    prometheus_parse::Value::Counter(value)
                    | prometheus_parse::Value::Gauge(value)
                    | prometheus_parse::Value::Untyped(value) => value,
                    _ => return None,
                };
                value.is_finite().then(|| Sample {
                    name: sample.metric,
                    labels: sample
                        .labels
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                    value,
                })
            })
            .collect();
        Ok(Snapshot::builder()
            .collected_at(chrono::Utc::now().timestamp())
            .history_available(self.upstream.is_some())
            .samples(samples)
            .process(self.process.clone())
            .build())
    }

    /// Executes an allowlisted, size-bounded history query against the configured upstream.
    pub async fn history(
        &self,
        query: HistoryQuery,
    ) -> ApiResult<serde_json::Value> {
        if !(60..=604800).contains(&query.seconds) {
            return RequestSnafu {
                stage: "metrics-history-window",
                code: ApiCode::COMMON_BAD_REQUEST,
            }
            .fail();
        }
        let upstream = self.upstream.as_ref().ok_or_else(|| {
            RequestSnafu {
                stage: "metrics-history-disabled",
                code: ApiCode::service_unavailable(5031003),
            }
            .build()
        })?;
        let outcome = async {
            let mut url = upstream.clone();
            url.set_path(&format!(
                "{}/api/v1/query_range",
                url.path().trim_end_matches('/')
            ));
            let end = chrono::Utc::now().timestamp();
            let mut response = self
                .client
                .get(url)
                .query(&[
                    ("query", query.chart.query().to_owned()),
                    ("start", (end - i64::from(query.seconds)).to_string()),
                    ("end", end.to_string()),
                    ("step", (query.seconds / 600).max(5).to_string()),
                    ("timeout", "5s".to_owned()),
                ])
                .send()
                .await?
                .error_for_status()?;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if body.len() + chunk.len() > 2 * 1024 * 1024 {
                    return Err("historical response exceeds two MiB".into());
                }
                body.extend_from_slice(&chunk);
            }
            let value: serde_json::Value = serde_json::from_slice(&body)?;
            let data = value.get("data").ok_or("missing Prometheus data")?;
            if value.get("status").and_then(serde_json::Value::as_str)
                != Some("success")
                || data.get("resultType").and_then(serde_json::Value::as_str)
                    != Some("matrix")
            {
                return Err("invalid Prometheus range response".into());
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(data.clone())
        }
        .await;
        outcome.map_err(|error| {
            tracing::warn!("historical metrics query failed: {}", error);
            RequestSnafu {
                stage: "metrics-history-upstream",
                code: ApiCode::service_unavailable(5031003),
            }
            .build()
        })
    }
}
impl Chart {
    /// Uses a fixed service selector and avoids summing replicated database backlog snapshots.
    fn query(self) -> &'static str {
        match self {
            Self::QueueWait => {
                "histogram_quantile(0.95, sum by(le, queue) (rate(docparse_queue_residence_seconds_bucket{service=\"docparse\",outcome=\"dequeued\"}[5m])))"
            }
            Self::AdmissionWait => {
                "histogram_quantile(0.95, sum by(le, queue, outcome) (rate(docparse_queue_admission_wait_seconds_bucket{service=\"docparse\"}[5m])))"
            }
            Self::PagePressure => {
                "sum(docparse_page_slots_used{service=\"docparse\"}) / sum(docparse_page_slots_capacity{service=\"docparse\"})"
            }
            Self::Oldest => {
                "max by(state) (docparse_job_oldest_age_seconds{service=\"docparse\"} and on(job, instance) (docparse_database_collection_success{service=\"docparse\"} == 1) and on(job, instance) ((time() - docparse_database_collection_timestamp_seconds{service=\"docparse\"}) <= 15))"
            }
            Self::Turnaround => {
                "histogram_quantile(0.95, sum by(le, outcome) (rate(docparse_job_end_to_end_seconds_bucket{service=\"docparse\"}[5m])))"
            }
            Self::Backlog => {
                "max by(state) (docparse_jobs{service=\"docparse\"} and on(job, instance) (docparse_database_collection_success{service=\"docparse\"} == 1) and on(job, instance) ((time() - docparse_database_collection_timestamp_seconds{service=\"docparse\"}) <= 15))"
            }
            Self::Queue => {
                "sum by(queue) (docparse_queue_items{service=\"docparse\"}) / sum by(queue) (docparse_queue_capacity_items{service=\"docparse\"})"
            }
            Self::Blocked => {
                "sum by(queue) (docparse_queue_blocked_producers{service=\"docparse\"})"
            }
            Self::Throughput => {
                "sum by(outcome) (rate(docparse_jobs_completed_total{service=\"docparse\"}[1m])) * 60"
            }
            Self::Wait => {
                "histogram_quantile(0.95, sum by(le) (rate(docparse_job_queue_wait_seconds_bucket{service=\"docparse\"}[5m])))"
            }
            Self::Parse => {
                "histogram_quantile(0.95, sum by(le) (rate(docparse_job_parse_seconds_bucket{service=\"docparse\"}[5m])))"
            }
            Self::Inference => {
                "histogram_quantile(0.95, sum by(le, model, graph) (rate(docparse_onnx_run_seconds_bucket{service=\"docparse\"}[5m])))"
            }
            Self::Workers => {
                "sum by(model) (docparse_model_workers_busy{service=\"docparse\"}) / sum by(model) (docparse_model_workers_configured{service=\"docparse\"})"
            }
            Self::Pages => {
                "sum(rate(docparse_pages_parsed_total{service=\"docparse\"}[1m])) * 60"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Executes the actual history expressions against fresh, failed, and stalled replica samples.
    #[test]
    #[ignore = "requires PROMTOOL pointing to a Prometheus promtool executable"]
    fn database_history_excludes_failed_and_stalled_replicas() {
        let mut input = String::new();
        for (instance, count, age, success, timestamp) in [
            ("fresh", 3, 11, "1+0x20", "0+5x20"),
            ("failed", 99, 1000, "1 0+0x19", "0+0x20"),
            ("stalled", 88, 900, "1+0x20", "0+0x20"),
        ] {
            for (metric, state, values) in [
                (
                    "docparse_jobs",
                    ",state=\"queued\"",
                    format!("{count}+0x20"),
                ),
                (
                    "docparse_job_oldest_age_seconds",
                    ",state=\"queued\"",
                    format!("{age}+0x20"),
                ),
                ("docparse_database_collection_success", "", success.into()),
                (
                    "docparse_database_collection_timestamp_seconds",
                    "",
                    timestamp.into(),
                ),
            ] {
                input.push_str(&format!("    - series: '{metric}{{service=\"docparse\",job=\"docparse\",instance=\"{instance}\"{state}}}'\n      values: '{values}'\n"));
            }
        }
        let mut cases = String::new();
        for (chart, value) in [(Chart::Backlog, 3), (Chart::Oldest, 11)] {
            let query = serde_json::to_string(chart.query()).expect("query");
            cases.push_str(&format!("    - expr: {query}\n      eval_time: 30s\n      exp_samples:\n        - labels: '{{state=\"queued\"}}'\n          value: {value}\n    - expr: {query}\n      eval_time: 120s\n      exp_samples: []\n"));
        }
        let file = tempfile::NamedTempFile::new().expect("test file");
        std::fs::write(file.path(), format!("evaluation_interval: 5s\ntests:\n  - interval: 5s\n    input_series:\n{input}    promql_expr_test:\n{cases}")).expect("test rules");
        let output = std::process::Command::new(
            std::env::var("PROMTOOL").expect("PROMTOOL"),
        )
        .args(["test", "rules"])
        .arg(file.path())
        .output()
        .expect("run promtool");
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
