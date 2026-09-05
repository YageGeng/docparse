use std::path::Path;

use docparse_config::ExecutionProviderConfig;
use ndarray::Ix2;
use ort::session::{
    HasSelectedOutputs, OutputSelector, RunOptions, Session, SessionOutputs,
};
use ort::value::TensorRef;

use crate::{LayoutDetection, LayoutError, PageTransform};

use super::postprocess::postprocess_page;
use super::preprocess::ModelInputs;
use super::schema::ModelSchema;

/// One mutable ONNX Runtime session owned by exactly one pool slot.
pub(crate) struct LayoutSession {
    session: Session,
    run_options: RunOptions<HasSelectedOutputs>,
}

impl LayoutSession {
    /// Loads one session with the selected execution provider and validates its schema.
    pub(crate) fn load(
        model_path: &Path,
        provider: ExecutionProviderConfig,
    ) -> Result<Self, LayoutError> {
        let builder = Session::builder()?;
        let mut builder = match provider {
            ExecutionProviderConfig::Cpu => builder,
            ExecutionProviderConfig::Cuda => {
                #[cfg(feature = "cuda")]
                {
                    builder
                        .with_execution_providers([
                            // A requested accelerator must fail visibly instead of silently using CPU.
                            ort::ep::CUDA::default().build().error_on_failure(),
                        ])
                        .map_err(|source| {
                            LayoutError::from(ort::Error::from(source))
                        })?
                }
                #[cfg(not(feature = "cuda"))]
                {
                    let _builder = builder;
                    return Err(LayoutError::ExecutionProviderUnavailable {
                        provider: "cuda",
                    });
                }
            }
            ExecutionProviderConfig::CoreMl => {
                #[cfg(feature = "coreml")]
                {
                    builder
                        .with_execution_providers([
                            // A requested accelerator must fail visibly instead of silently using CPU.
                            ort::ep::CoreML::default()
                                .build()
                                .error_on_failure(),
                        ])
                        .map_err(|source| {
                            LayoutError::from(ort::Error::from(source))
                        })?
                }
                #[cfg(not(feature = "coreml"))]
                {
                    let _builder = builder;
                    return Err(LayoutError::ExecutionProviderUnavailable {
                        provider: "coreml",
                    });
                }
            }
            ExecutionProviderConfig::Openvino => {
                #[cfg(feature = "openvino")]
                {
                    builder
                        .with_execution_providers([
                            // A requested accelerator must fail visibly instead of silently using CPU.
                            ort::ep::OpenVINO::default()
                                .build()
                                .error_on_failure(),
                        ])
                        .map_err(|source| {
                            LayoutError::from(ort::Error::from(source))
                        })?
                }
                #[cfg(not(feature = "openvino"))]
                {
                    let _builder = builder;
                    return Err(LayoutError::ExecutionProviderUnavailable {
                        provider: "openvino",
                    });
                }
            }
        };
        let session = builder.commit_from_file(model_path)?;
        ModelSchema::from_session(&session)?.validate_pp_doclayout_v3()?;
        // Runtime postprocessing consumes only boxes and their valid row count. Excluding masks
        // avoids materializing a 300x200x200 i32 tensor for every page and lets ORT prune it.
        let run_options = RunOptions::new()?.with_outputs(
            OutputSelector::no_default()
                .with("fetch_name_0")
                .with("fetch_name_1"),
        );
        Ok(Self {
            session,
            run_options,
        })
    }

    /// Runs inference and converts all validated page outputs before releasing ORT values.
    pub(crate) fn detect(
        &mut self,
        inputs: &ModelInputs,
        transform: &PageTransform,
        threshold: f64,
    ) -> Result<Vec<LayoutDetection>, LayoutError> {
        let outputs = self.run(inputs)?;
        let bbox = outputs
            .get("fetch_name_0")
            .ok_or(LayoutError::MissingOutput {
                name: "fetch_name_0",
            })?
            .try_extract_array::<f32>()?
            .into_dimensionality::<Ix2>()
            .map_err(|source| LayoutError::InvalidOutput {
                name: "fetch_name_0",
                reason: source.to_string(),
            })?;
        let bbox_num = outputs
            .get("fetch_name_1")
            .ok_or(LayoutError::MissingOutput {
                name: "fetch_name_1",
            })?
            .try_extract_array::<i32>()?;
        let Some(bbox_count) = bbox_num.iter().next().copied() else {
            return Err(LayoutError::InvalidOutput {
                name: "fetch_name_1",
                reason: "expected one bbox count".to_owned(),
            });
        };
        if bbox_num.len() != 1 {
            return Err(LayoutError::InvalidOutput {
                name: "fetch_name_1",
                reason: format!(
                    "expected one bbox count, got {}",
                    bbox_num.len()
                ),
            });
        }
        postprocess_page(bbox, bbox_count, threshold, transform)
            .map_err(LayoutError::from)
    }

    /// Executes the fixed named three-input graph without copying ndarray storage.
    fn run<'session>(
        &'session mut self,
        inputs: &ModelInputs,
    ) -> Result<SessionOutputs<'session>, LayoutError> {
        let outputs = self.session.run_with_options(ort::inputs! {
            "im_shape" => TensorRef::from_array_view(&inputs.image_size)?,
            "image" => TensorRef::from_array_view(&inputs.image)?,
            "scale_factor" => TensorRef::from_array_view(&inputs.scale_factor)?,
        }, &self.run_options)?;
        Ok(outputs)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use docparse_config::ExecutionProviderConfig;
    use ndarray::{Array2, Array4};

    use super::{LayoutSession, ModelInputs};

    /// Verifies inference materializes only outputs consumed by layout postprocessing.
    #[test]
    #[ignore = "requires fixed PP-DocLayoutV3 model"]
    fn session_requests_only_consumed_outputs() {
        let model_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("models/pp-doclayout-v3/inference.onnx");
        let mut session =
            LayoutSession::load(&model_path, ExecutionProviderConfig::Cpu)
                .expect("the fixed model must load");
        let inputs = ModelInputs {
            image: Array4::zeros((1, 3, 800, 800)),
            image_size: Array2::from_shape_vec((1, 2), vec![800.0, 800.0])
                .expect("image size must have the fixed model shape"),
            scale_factor: Array2::from_shape_vec((1, 2), vec![1.0, 1.0])
                .expect("scale factor must have the fixed model shape"),
        };

        let outputs = session.run(&inputs).expect("inference must succeed");

        assert!(outputs.get("fetch_name_0").is_some());
        assert!(outputs.get("fetch_name_1").is_some());
        assert!(outputs.get("fetch_name_2").is_none());
    }
}
