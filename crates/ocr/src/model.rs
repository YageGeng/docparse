//! Pinned OCR contracts and bounded output readback shared by every execution provider.
use crate::{OcrError, decode::CtcSteps, detect::DetectionMap};
use docparse_common::timing::TimingStage;
use docparse_layout::ModelContract;
use ndarray::{Ix2, Ix3, Ix4};
use ort::session::{Session, SessionOutputs};

/// Each stage has one immutable model contract and one expected tensor family.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ModelKind {
    Detection,
    Recognition,
    Orientation,
}

/// Compact owned output remains valid after the session or browser readback guard is released.
pub(crate) enum ModelOutput {
    Detection(DetectionMap),
    Recognition(Vec<CtcSteps>),
    Orientation(Vec<Orientation>),
}

/// One classifier result retains its position in the submitted line batch.
pub(crate) struct Orientation {
    pub rotated: bool,
    pub confidence: f64,
}

impl ModelKind {
    /// Returns the exact official Paddle artifact identity accepted for this stage.
    pub fn contract(self) -> ModelContract {
        let (repository, revision, model, config) = match self {
            Self::Detection => (
                "PaddlePaddle/PP-OCRv6_medium_det_onnx",
                "61323801669c338b7891481ec7bac61ce31b576a",
                "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1",
                "7298d5ead546584af2504d03355f881ac7a7bc0eb1e282d3e159277c1d0af871",
            ),
            Self::Recognition => (
                "PaddlePaddle/PP-OCRv6_medium_rec_onnx",
                "50c7eacafc52fa7bcf4194e8cd08e46f8558504b",
                "9c09abf0957f7968c7586464b7397b84ad2387a0497a351af40e9acc71b673ba",
                "991b700facf5b50a7de193468207d5f4255b538dde0d312ae3b7c7a9b6873129",
            ),
            Self::Orientation => (
                "PaddlePaddle/PP-LCNet_x1_0_textline_ori_onnx",
                "7fdcf3cf7061163eda7183b224aa334bd33068f7",
                "38aa97cd4be591e0ad304e659f07ba30d946f27a63315433f6659c69c8778345",
                "8d5120d0e1a30a9df7ed46aa9119da3796ed066777089d1c1d705f132d5e90f9",
            ),
        };
        ModelContract::builder()
            .repository(repository.into())
            .revision(revision.into())
            .license("Apache-2.0".into())
            .model_sha256(model.into())
            .config_sha256(config.into())
            .build()
    }

    /// Rejects a different graph interface before any request reaches ONNX Runtime.
    pub fn validate_session(self, session: &Session) -> Result<(), OcrError> {
        if session.inputs().len() != 1
            || session
                .inputs()
                .first()
                .is_none_or(|input| input.name() != "x")
            || session.outputs().len() != 1
        {
            return Err(OcrError::InvalidModel(format!(
                "{self:?} requires one x input and one output"
            )));
        }
        Ok(())
    }

    /// Distinguishes real detector, classifier and recognizer execution in public timing observations.
    pub fn timing(self) -> TimingStage {
        match self {
            Self::Detection => TimingStage::OcrDetectionInference,
            Self::Recognition => TimingStage::OcrRecognitionInference,
            Self::Orientation => TimingStage::OcrOrientationInference,
        }
    }

    /// Validates batch correspondence and reduces each borrowed recognition output before releasing the session.
    pub fn read(
        self,
        outputs: &SessionOutputs<'_>,
        batch_size: usize,
    ) -> Result<Vec<ModelOutput>, OcrError> {
        let (_, output) = outputs.iter().next().ok_or_else(|| {
            OcrError::InvalidData("OCR returned no output".into())
        })?;
        let array = output.try_extract_array::<f32>()?;
        if !(1..=32).contains(&batch_size)
            || array.shape().first() != Some(&batch_size)
        {
            return Err(OcrError::InvalidData(
                "OCR output batch does not match input".into(),
            ));
        }
        let shape_error = |error: ndarray::ShapeError| {
            OcrError::InvalidData(error.to_string())
        };
        match self {
            Self::Detection => {
                let array =
                    array.into_dimensionality::<Ix4>().map_err(shape_error)?;
                let (_batch, channels, height, width) = array.dim();
                if channels != 1
                    || height == 0
                    || width == 0
                    || height > 4096
                    || width > 4096
                {
                    return Err(OcrError::InvalidData(
                        "invalid [B,1,H,W] detector output".into(),
                    ));
                }
                Ok(array
                    .outer_iter()
                    .map(|page| {
                        ModelOutput::Detection(DetectionMap(
                            page.index_axis(ndarray::Axis(0), 0).to_owned(),
                        ))
                    })
                    .collect())
            }

            Self::Recognition => {
                let array =
                    array.into_dimensionality::<Ix3>().map_err(shape_error)?;
                array
                    .outer_iter()
                    .map(|row| {
                        CtcSteps::try_from(row)
                            .map(|steps| ModelOutput::Recognition(vec![steps]))
                    })
                    .collect()
            }

            Self::Orientation => {
                let array =
                    array.into_dimensionality::<Ix2>().map_err(shape_error)?;
                if array.dim() != (batch_size, 2)
                    || array.iter().any(|score| {
                        !score.is_finite() || !(0.0..=1.0).contains(score)
                    })
                {
                    return Err(OcrError::InvalidData(
                        "invalid [B,2] orientation output".into(),
                    ));
                }
                let normal = array.column(0);
                let rotated = array.column(1);
                Ok(normal
                    .iter()
                    .zip(rotated.iter())
                    .map(|(&normal, &rotated)| {
                        ModelOutput::Orientation(vec![Orientation {
                            rotated: rotated > normal,
                            confidence: f64::from(normal.max(rotated)),
                        }])
                    })
                    .collect())
            }
        }
    }
}
