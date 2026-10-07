//! Immutable structure and cell model artifacts share the existing verification contract.
use docparse_common::timing::TimingStage;
use docparse_config::{TableCellModel, TsrModel};
use docparse_layout::{
    ModelArtifacts, ModelContract, ProfileDim, TensorRtProfile,
};

/// Files required by the selected structure model and optional cell detector.
#[derive(Debug, Clone)]
pub struct TsrArtifacts {
    pub structure: ModelArtifacts,
    pub cell_detection: Option<ModelArtifacts>,
}

/// Square input edge of both RT-DETR table-cell detectors.
pub(crate) const CELL_EDGE: usize = 640;
/// Long edge TATR scales tables to, keeping their aspect ratio.
pub(crate) const TATR_EDGE: usize = 800;

impl From<ModelArtifacts> for TsrArtifacts {
    /// Keeps the single-model artifact constructor compatible with SLANet+ callers.
    fn from(structure: ModelArtifacts) -> Self {
        Self {
            structure,
            cell_detection: None,
        }
    }
}

/// Selects the tensor contract for each real session in the TSR pipeline.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ModelKind {
    Structure(TsrModel),
    Cells(TableCellModel),
}

impl ModelKind {
    /// Identifies independent structure and cell queues without request-specific labels.
    pub(crate) fn metric_name(self) -> &'static str {
        match self {
            Self::Structure(_) => "tsr_structure",
            Self::Cells(_) => "tsr_cells",
        }
    }

    /// Matches approved immutable model artifacts, including the verified local TATR export.
    pub(crate) fn contract(self) -> ModelContract {
        let (repository, revision, model, config) = match self {
            Self::Structure(TsrModel::SlanetPlus) => (
                "SLANet_plus_onnx",
                "7dbe640e127602bf506815e822c09758de73c482",
                "7790c0c13ce064782c9d22ebeb16b4da8216f83d3ba576da962c106ef58386da",
                "8a6372d3269a6f112fe13a2da7952a84da6e112c10a3146cbb43de5bd01d19fa",
            ),
            Self::Structure(TsrModel::SlanextWired) => (
                "SLANeXt_wired_onnx",
                "04356de883011f433f83e5098793f3a501a9af6e",
                "0a6e063b56e35a434eb6669eb2342113c6bd76a6ce5acaa0331f370c9e00732f",
                "abbbd1b4dc6b1a2e9cd34c035514da53a1a6b1ec267292b0b8802025650a33bf",
            ),
            Self::Structure(TsrModel::SlanextWireless) => (
                "SLANeXt_wireless_onnx",
                "9207aaed01d1bbb0743af384bac5b0bd35869ba3",
                "5c79ee87cce6712f8f640394decce72157bd1df13c9bccf86d071bd07a6e9f97",
                "58d1d7fdffd3e58cfec98571b817ea012f2107d644bd4f8e4607fae84f1923a6",
            ),
            // Keep the local export's identity separate from official Paddle artifacts.
            Self::Structure(TsrModel::Tatr) => {
                return ModelContract::builder()
                .repository("microsoft/table-transformer-structure-recognition-v1.1-all".to_owned())
                .revision("7587a7ef111d9dcbf8ac695f1376ab7014340a0c".to_owned())
                .license("MIT".to_owned())
                .model_sha256("ef7b679634f4693f4c0b6eecd1cd255d9cc5b844a3e5802b5e5456c8b9f1820e".to_owned())
                .config_sha256("eead409bb80e36ae85b8377642c54550f0504f65688ba3a4967950cafe461df2".to_owned())
                .build();
            }
            Self::Cells(TableCellModel::Wired) => (
                "RT-DETR-L_wired_table_cell_det_onnx",
                "b2c0720b5fe6f1c0dd40f8a7993a3f28e04252f8",
                "bf5490020512a31f43813d90feadae9526a2c3474ffe807571f2c23594f5958f",
                "edf6d6180f2b9e3e666c744ee5ded38a72c6ef9056cd193250e3e55ba268acef",
            ),
            Self::Cells(TableCellModel::Wireless) => (
                "RT-DETR-L_wireless_table_cell_det_onnx",
                "94c021be206064f0136ef1383fbd4b68b168fa61",
                "47515940ec5c37156e09aa9acb20c4e7e22456cad6ce49473661f1762fb46a78",
                "f2d0f00ea42aacc162f72a35cf54330e392a7d669e9a1b43896d3bd77a512621",
            ),
        };
        ModelContract::builder()
            .repository(format!("PaddlePaddle/{repository}"))
            .revision(revision.to_owned())
            .license("Apache-2.0".to_owned())
            .model_sha256(model.to_owned())
            .config_sha256(config.to_owned())
            .build()
    }

    /// Specializes the structure graph to its actual preprocessed dimensions.
    pub(crate) fn edge(self) -> usize {
        // Only models with a TensorRT profile share a named constant with their preprocessing.
        match self {
            Self::Structure(TsrModel::SlanetPlus) => 488,
            Self::Structure(TsrModel::Tatr) => TATR_EDGE,
            Self::Structure(_) => 512,
            Self::Cells(_) => CELL_EDGE,
        }
    }

    /// Shape profile so TensorRT builds one engine per model; TATR keeps its aspect ratio, so
    /// both spatial axes range up to its long edge. SLANet+ and SLANeXt return `None`: their
    /// autoregressive Paddle `Loop` fails TensorRT's recurrence-shape validation at engine build.
    pub(crate) fn tensorrt_profile(
        self,
        max_batch: usize,
    ) -> Option<TensorRtProfile> {
        // Wide tables are the common case, so the optimization target is 600x800.
        const TATR_SIDE: ProfileDim = ProfileDim::Range {
            min: 1,
            opt: TATR_EDGE,
            max: TATR_EDGE,
        };
        const TATR_HEIGHT: ProfileDim = ProfileDim::Range {
            min: 1,
            opt: 600,
            max: TATR_EDGE,
        };
        static TATR: [(&str, &[ProfileDim]); 2] = [
            (
                "pixel_values",
                &[ProfileDim::Fixed(3), TATR_HEIGHT, TATR_SIDE],
            ),
            ("pixel_mask", &[TATR_HEIGHT, TATR_SIDE]),
        ];
        static CELLS: [(&str, &[ProfileDim]); 3] = [
            ("im_shape", &[ProfileDim::Fixed(2)]),
            (
                "image",
                &[
                    ProfileDim::Fixed(3),
                    ProfileDim::Fixed(CELL_EDGE),
                    ProfileDim::Fixed(CELL_EDGE),
                ],
            ),
            ("scale_factor", &[ProfileDim::Fixed(2)]),
        ];
        let inputs: &'static [(&str, &[ProfileDim])] = match self {
            Self::Structure(TsrModel::Tatr) => &TATR,
            Self::Structure(_) => return None,
            Self::Cells(_) => &CELLS,
        };
        Some(
            TensorRtProfile::builder()
                .inputs(inputs)
                .max_batch(max_batch)
                .model(&self.contract().model_sha256)
                .build(),
        )
    }

    /// Keeps detection execution separate from structure execution in measurements.
    pub(crate) fn timing(self) -> TimingStage {
        match self {
            Self::Structure(_) => TimingStage::TsrInference,
            Self::Cells(_) => TimingStage::TableCellInference,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TensorRT rejects the SLANet+/SLANeXt `Loop` decoders, so only TATR and the cell detectors
    /// may declare a profile; a profile on a rejected graph would fail every TensorRT start.
    #[test]
    fn only_tensorrt_compatible_models_declare_profiles() {
        for (kind, profiled) in [
            (ModelKind::Structure(TsrModel::Tatr), true),
            (ModelKind::Structure(TsrModel::SlanetPlus), false),
            (ModelKind::Structure(TsrModel::SlanextWired), false),
            (ModelKind::Structure(TsrModel::SlanextWireless), false),
            (ModelKind::Cells(TableCellModel::Wired), true),
            (ModelKind::Cells(TableCellModel::Wireless), true),
        ] {
            assert_eq!(
                kind.tensorrt_profile(1).is_some(),
                profiled,
                "{kind:?}"
            );
        }
    }
}
