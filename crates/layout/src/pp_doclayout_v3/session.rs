use crate::LayoutError;
use ndarray::{Array2, Ix2};
use ort::session::SessionOutputs;

/// Owned model results after platform-specific output synchronization.
pub(crate) struct ModelOutputs {
    pub(crate) boxes: Array2<f32>,
    pub(crate) count: i32,
}

impl TryFrom<&SessionOutputs<'_>> for ModelOutputs {
    type Error = LayoutError;

    /// Validates the consumed tensor shapes before copying their small owned result.
    fn try_from(outputs: &SessionOutputs<'_>) -> Result<Self, Self::Error> {
        let boxes = outputs
            .get("fetch_name_0")
            .ok_or(LayoutError::MissingOutput {
                name: "fetch_name_0",
            })?
            .try_extract_array::<f32>()?
            .into_dimensionality::<Ix2>()
            .map_err(|error| LayoutError::InvalidOutput {
                name: "fetch_name_0",
                reason: error.to_string(),
            })?;
        let counts = outputs
            .get("fetch_name_1")
            .ok_or(LayoutError::MissingOutput {
                name: "fetch_name_1",
            })?
            .try_extract_array::<i32>()?;
        let count = counts.iter().next().copied().ok_or(
            LayoutError::InvalidOutput {
                name: "fetch_name_1",
                reason: "expected one bbox count".into(),
            },
        )?;
        if counts.len() != 1
            || boxes.ncols() != 7
            || count < 0
            || usize::try_from(count).unwrap_or(usize::MAX) > boxes.nrows()
        {
            return Err(LayoutError::InvalidOutput {
                name: "fetch_name_0",
                reason: "box shape or valid row count violates model contract"
                    .into(),
            });
        }
        Ok(Self {
            boxes: boxes.to_owned(),
            count,
        })
    }
}
