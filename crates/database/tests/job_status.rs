use docparse_database::{JobStatus, seaorm::ActiveEnum};

/// Typed states must retain both the deployed varchar values and the public JSON spelling.
#[test]
fn task_states_preserve_storage_and_wire_values() {
    for (status, value, terminal) in [
        (JobStatus::Queued, "queued", false),
        (JobStatus::Running, "running", false),
        (JobStatus::Succeeded, "succeeded", true),
        (JobStatus::Failed, "failed", true),
    ] {
        assert_eq!(status.to_value(), value);
        assert_eq!(serde_json::to_value(status).expect("serialize"), value);
        assert_eq!(
            JobStatus::try_from_value(&value.to_owned()).expect("stored state"),
            status
        );
        assert_eq!(status.is_terminal(), terminal);
    }
    serde_json::from_str::<JobStatus>("\"unknown\"")
        .expect_err("reject unknown state");
}
