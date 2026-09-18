use docparse_config::OcrConfig;
use docparse_ocr::OcrArtifacts;

/// Explicit filenames must reach every OCR model, while disabled orientation must not read its files.
#[tokio::test]
async fn configured_files_are_loaded_without_directory_conventions() {
    let directory = tempfile::tempdir().expect("artifact directory");
    let mut config = OcrConfig::default();
    for (name, files) in [
        ("detect", &mut config.detection.files),
        ("recognize", &mut config.recognition.files),
        ("orient", &mut config.orientation.files),
    ] {
        files.model_path = directory.path().join(format!("{name}.onnx"));
        files.model_config_path = directory.path().join(format!("{name}.yaml"));
        files.model_manifest_path =
            directory.path().join(format!("{name}.json"));
        // Distinct bytes detect a mix-up between model families or between their three file roles.
        for (path, role) in [
            (&files.model_path, "model"),
            (&files.model_config_path, "config"),
            (&files.model_manifest_path, "manifest"),
        ] {
            std::fs::write(path, format!("{name}-{role}")).expect("artifact");
        }
    }
    let artifacts = OcrArtifacts::from_config(&config)
        .await
        .expect("configured files");
    for (name, files) in [
        ("detect", artifacts.detection),
        ("recognize", artifacts.recognition),
        ("orient", artifacts.orientation.expect("orientation")),
    ] {
        assert_eq!(&*files.model, format!("{name}-model").as_bytes());
        assert_eq!(&*files.config, format!("{name}-config").as_bytes());
        assert_eq!(&*files.manifest, format!("{name}-manifest").as_bytes());
    }
    std::fs::remove_file(&config.orientation.files.model_path)
        .expect("remove orientation");
    assert!(matches!(
        OcrArtifacts::from_config(&config).await,
        Err(docparse_ocr::OcrError::Artifacts(docparse_layout::ModelManifestError::Read { path, .. }))
            if path == config.orientation.files.model_path
    ));
    config.classify_orientation = false;
    assert!(
        OcrArtifacts::from_config(&config)
            .await
            .expect("disabled orientation")
            .orientation
            .is_none()
    );
}
