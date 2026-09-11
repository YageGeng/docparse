//! Table fixtures tests and support, compiled only within the parent test module.
use super::*;

/// A real model response and its immutable native source facts.
#[derive(serde::Deserialize)]
pub(super) struct CapturedTable {
    pub(super) source: serde_json::Value,
    pub(super) prediction: serde_json::Value,
    pub(super) size: [u32; 2],
}

impl CapturedTable {
    /// Runs the production adapter and assembler without executing inference again.
    #[allow(
        clippy::indexing_slicing,
        reason = "the recorded fixture schema requires these validated fields"
    )]
    pub(super) fn reconstruct(
        &self,
    ) -> Result<Block, crate::TableStructureError> {
        use docparse_layout::{PageImage, PageImageInput, PixelFormat};
        let value = &self.source;
        let prediction = &self.prediction;
        let mut block: Block = serde_json::from_value(value["block"].clone())
            .expect("source block");
        block.table = None;
        if let Some(error) = prediction["prediction"].get("error") {
            return Err(crate::TableStructureError::Engine {
                message: error.to_string(),
            });
        }
        let [width, height] = self.size;
        let request = crate::TsrTableRequest::builder()
            .request_id(
                prediction["request_id"]
                    .as_str()
                    .expect("request id")
                    .to_owned(),
            )
            .page_number(prediction["page"].as_u64().expect("page") as u32)
            .block_id(block.id.clone())
            .crop_bbox(
                serde_json::from_value(prediction["bbox"].clone())
                    .expect("crop bounds"),
            )
            .crop_to_viewport(
                serde_json::from_value(prediction["transform"].clone())
                    .expect("crop transform"),
            )
            .image(std::sync::Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(width)
                        .height(height)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(std::sync::Arc::from(vec![
                            255;
                            width as usize
                                * height as usize
                                * 3
                        ]))
                        .build(),
                )
                .expect("owned pixels"),
            ))
            .reason(crate::TsrRequestReason::ExternalOnly)
            .build();
        let mut evidence = TableEvidence::default();
        for word in value["words"].as_array().expect("source words") {
            let span: TableTextSpan =
                serde_json::from_value(word["span"].clone())
                    .expect("source span");
            evidence.words.entry(span.text_item_id).or_default().push(
                crate::table::TableWord::builder()
                    .byte_range(span.byte_range)
                    .bbox(span.bbox)
                    .baseline(
                        serde_json::from_value(
                            word["measured_baseline"].clone(),
                        )
                        .expect("baseline"),
                    )
                    .build(),
            );
        }
        let rules: Vec<(String, f64, f64, f64)> = serde_json::from_value(
            value
                .get("rules")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .expect("source rules");
        evidence.rules = rules
            .into_iter()
            .map(|(kind, at, start, end)| {
                if kind == "h" {
                    crate::table::TableRule::Horizontal {
                        y: at,
                        left: start,
                        right: end,
                    }
                } else {
                    crate::table::TableRule::Vertical {
                        x: at,
                        top: start,
                        bottom: end,
                    }
                }
            })
            .collect();
        let model = docparse_tsr::TsrPrediction {
            structure_tokens: serde_json::from_value(
                prediction["prediction"]["structure_tokens"].clone(),
            )
            .expect("model tokens"),
            cell_bboxes: serde_json::from_value(
                prediction["prediction"]["cell_bboxes"].clone(),
            )
            .expect("model boxes"),
            score: prediction["prediction"]["score"]
                .as_f64()
                .expect("model score"),
        };
        let input = crate::TsrTableInput::from((&request, model));
        TableAssembler::new(&FusionConfig::default(), &evidence, &[])
            .reconstruct_external(
                &mut block,
                &request,
                input,
                crate::TsrGeometryPolicy::Predicted,
            )?;
        Ok(block)
    }
}
