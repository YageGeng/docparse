use std::sync::Arc;

use docparse_layout::{ModelArtifacts, ModelManifestError};

/// Rejects altered model bytes even when the manifest claims the approved model identity.
#[test]
fn artifact_bytes_cannot_bypass_the_fixed_hash() {
    let manifest = br#"{
        "repository":"PaddlePaddle/PP-DocLayoutV3_onnx",
        "revision":"46bbdf188bb0a772c08aed74882ce7e51a8f1ea6",
        "license":"Apache-2.0",
        "files":{
            "inference.onnx":"45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba",
            "inference.yml":"506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc"
        }
    }"#;
    let artifacts = ModelArtifacts {
        model: Arc::from(&b"corrupt model"[..]),
        config: Arc::from(&b""[..]),
        manifest: Arc::from(manifest.as_slice()),
    };
    assert!(matches!(
        artifacts.verify(),
        Err(ModelManifestError::ContentHashMismatch {
            artifact: "inference.onnx",
            ..
        })
    ));
}
