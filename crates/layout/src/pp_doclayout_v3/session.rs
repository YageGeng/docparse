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
        Self::from_batch(outputs, 1)?
            .pop()
            .ok_or(LayoutError::InvalidOutput {
                name: "fetch_name_1",
                reason: "missing layout result".into(),
            })
    }
}

impl ModelOutputs {
    /// Splits concatenated boxes using per-page counts so each caller retains only its own detections.
    pub(crate) fn from_batch(
        outputs: &SessionOutputs<'_>,
        batch: usize,
    ) -> Result<Vec<Self>, LayoutError> {
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
        if counts.len() != batch || boxes.ncols() != 7 {
            return Err(LayoutError::InvalidOutput {
                name: "fetch_name_0",
                reason: "box shape or valid row count violates model contract"
                    .into(),
            });
        }
        let mut offset = 0usize;
        counts
            .iter()
            .map(|&count| {
                let end = usize::try_from(count)
                    .ok()
                    .and_then(|count| offset.checked_add(count))
                    .filter(|&end| end <= boxes.nrows())
                    .ok_or(LayoutError::InvalidOutput {
                        name: "fetch_name_1",
                        reason: "layout bbox counts exceed the output buffer"
                            .into(),
                    })?;
                let result = Self {
                    boxes: boxes.slice(ndarray::s![offset..end, ..]).to_owned(),
                    count,
                };
                offset = end;
                Ok(result)
            })
            .collect()
    }
}
