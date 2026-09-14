//! Adapts the independent model crate to the existing source-preserving table pipeline.
use super::{
    TableStructureEngine, TableStructureError, TsrTableInput, TsrTableRequest,
};

impl TableStructureEngine for docparse_tsr::SlanetPlusEngine {
    /// Identifies the pinned model and selected backend in table evidence and logs.
    fn name(&self) -> &str {
        docparse_tsr::PaddleTsrEngine::name(self)
    }

    /// Position-head boxes are approximate and may align to nearby native ink gaps.
    fn geometry_policy(&self) -> super::TsrGeometryPolicy {
        super::TsrGeometryPolicy::Predicted
    }

    /// Supplies topology and pixel geometry only; canonical PDF/OCR text is filled by core.
    fn recognize(
        &self,
        request: TsrTableRequest,
    ) -> crate::WasmBoxedFuture<'_, Result<TsrTableInput, TableStructureError>>
    {
        Box::pin(async move {
            let prediction = self
                .predict(
                    std::sync::Arc::clone(&request.image),
                    request.timings.clone(),
                )
                .await
                .map_err(|error| TableStructureError::Engine {
                    message: error.to_string(),
                })?;
            Ok(TsrTableInput::from((&request, prediction)))
        })
    }
}

impl From<(&TsrTableRequest, docparse_tsr::TsrPrediction)> for TsrTableInput {
    /// Transfers raw model output; shared core decoding owns normalization and validation.
    fn from(
        (request, prediction): (&TsrTableRequest, docparse_tsr::TsrPrediction),
    ) -> Self {
        Self::builder()
            .request_id(request.request_id.clone())
            .structure_tokens(prediction.structure_tokens)
            .cell_bboxes(prediction.cell_bboxes)
            .detected_cell_bboxes(prediction.detected_cell_bboxes)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::{
        AffineTransform, Bbox, PageImage, PageImageInput, PixelFormat,
    };
    use std::sync::Arc;

    /// Independent position-head boxes remain available to source matching without imposing global dividers.
    #[test]
    fn model_boxes_are_preserved_for_source_matching() {
        let request = TsrTableRequest::builder()
            .request_id("model".to_owned())
            .page_number(1)
            .block_id(crate::BlockId::model(1, 0, 0))
            .crop_bbox(Bbox::try_from([0.0, 0.0, 100.0, 30.0]).expect("crop"))
            .image(Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(100)
                        .height(30)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(vec![255_u8; 9000]))
                        .build(),
                )
                .expect("image"),
            ))
            .crop_to_viewport(AffineTransform::identity())
            .reason(super::super::TsrRequestReason::TsrOnly)
            .build();
        let tokens = [
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let output = TsrTableInput::from((
            &request,
            docparse_tsr::TsrPrediction::builder()
                .structure_tokens(tokens.clone())
                .cell_bboxes(vec![
                    vec![0.0, 0.0, 60.0, 15.0],
                    vec![45.0, 0.0, 100.0, 15.0],
                    vec![0.0, 10.0, 60.0, 30.0],
                    vec![45.0, 10.0, 100.0, 30.0],
                ])
                .score(0.99)
                .build(),
        ));
        let grid = crate::table::grid::CellGrid::try_from((
            &request,
            request.crop_bbox,
            output.clone(),
            super::super::TsrGeometryPolicy::Declared,
        ))
        .expect("bounded topology");
        assert_eq!((grid.table().row_count, grid.table().column_count), (2, 2));
        assert_eq!(
            output.cell_bboxes,
            [
                vec![0.0, 0.0, 60.0, 15.0],
                vec![45.0, 0.0, 100.0, 15.0],
                vec![0.0, 10.0, 60.0, 30.0],
                vec![45.0, 10.0, 100.0, 30.0],
            ]
        );
        // A long label in one row must not overwrite geometry in other rows.
        let tokens = (0..3)
            .flat_map(|_| ["<tr>", "<td></td>", "<td></td>", "</tr>"])
            .map(str::to_owned)
            .collect();
        let output = TsrTableInput::from((
            &request,
            docparse_tsr::TsrPrediction::builder()
                .structure_tokens(tokens)
                .cell_bboxes(vec![
                    vec![0.0, 0.0, 20.0, 10.0],
                    vec![70.0, 0.0, 100.0, 10.0],
                    vec![0.0, 10.0, 60.0, 20.0],
                    vec![70.0, 10.0, 100.0, 20.0],
                    vec![0.0, 20.0, 20.0, 30.0],
                    vec![70.0, 20.0, 100.0, 30.0],
                ])
                .score(0.99)
                .build(),
        ));
        assert_eq!(
            output.cell_bboxes,
            [
                vec![0.0, 0.0, 20.0, 10.0],
                vec![70.0, 0.0, 100.0, 10.0],
                vec![0.0, 10.0, 60.0, 20.0],
                vec![70.0, 10.0, 100.0, 20.0],
                vec![0.0, 20.0, 20.0, 30.0],
                vec![70.0, 20.0, 100.0, 30.0]
            ]
        );
    }
}
