//! Adapter from image-pixel Paddle predictions into canonical DocParse viewport facts.
use crate::{OcrEngine, OcrError, OcrRequest, OcrResult, OcrTextItem};
use docparse_layout::{Bbox, Point, Quad};
use std::collections::BTreeMap;

impl OcrEngine for docparse_ocr::PaddleOcrEngine {
    /// Reports the actual built-in model family rather than an adapter-library identity.
    fn name(&self) -> &str {
        "pp-ocrv6-medium"
    }

    /// Runs real OCR and uses the exact render transform instead of assuming the requested DPI was achieved.
    fn recognize(
        &self,
        request: OcrRequest,
    ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
        '_,
        Result<OcrResult, OcrError>,
    > {
        Box::pin(async move {
            let regions = request
                .missing_regions
                .iter()
                .map(|bbox| {
                    let a = request
                        .transform
                        .viewport_to_rendered(Point::new(bbox.left, bbox.top));
                    let b = request.transform.viewport_to_rendered(Point::new(
                        bbox.right,
                        bbox.bottom,
                    ));
                    Bbox::try_from([a.x, a.y, b.x, b.y]).map_err(|error| {
                        OcrError::Engine {
                            message: error.to_string(),
                        }
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let predictions = docparse_ocr::PaddleOcrEngine::recognize(
                self,
                request.image,
                regions,
                request.timings,
            )
            .await
            .map_err(|error| OcrError::Engine {
                message: error.to_string(),
            })?;
            let items = predictions
                .into_iter()
                .map(|line| {
                    let quad =
                        Quad::try_from(line.quad.points().map(|point| {
                            request.transform.rendered_to_viewport(point)
                        }))
                        .map_err(|error| {
                            OcrError::Engine {
                                message: error.to_string(),
                            }
                        })?;
                    let polygon = docparse_layout::Polygon::from(quad);
                    Ok(OcrTextItem::builder()
                        .text(line.text)
                        .bbox(polygon.bbox())
                        .polygon(Some(polygon))
                        .confidence(line.confidence)
                        .build())
                })
                .collect::<Result<Vec<_>, OcrError>>()?;
            Ok(OcrResult::builder()
                .items(items)
                .metadata(BTreeMap::from([(
                    "engine".into(),
                    self.name().into(),
                )]))
                .build())
        })
    }
}
