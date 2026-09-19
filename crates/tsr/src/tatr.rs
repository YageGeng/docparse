//! Microsoft TATR tensor adaptation and geometry-only table topology reconstruction.
//! Text assignment and independent cell detection remain in the existing pipeline.
use crate::{TsrError, TsrPrediction};
use docparse_layout::PageImage;
use ndarray::{Array2, Array3, Array4, Axis, Ix3};
use ort::session::SessionOutputs;
use std::ops::Range;

/// RGB pixels and the valid-image mask travel together through shared model queues.
pub(crate) struct TatrInput {
    pub pixels: Array4<f32>,
    pub mask: Array3<i64>,
}

impl TryFrom<&PageImage> for TatrInput {
    type Error = TsrError;

    /// Resizes the longest edge to 800 with antialiased bilinear interpolation and RGB normalization.
    #[allow(
        clippy::cast_sign_loss,
        reason = "image dimensions and resize scale are nonnegative"
    )]
    fn try_from(image: &PageImage) -> Result<Self, Self::Error> {
        let scale = 800.0 / f64::from(image.width().max(image.height()));
        let width = (f64::from(image.width()) * scale).round_ties_even() as u32;
        let height =
            (f64::from(image.height()) * scale).round_ties_even() as u32;
        if width == 0 || height == 0 || width > 800 || height > 800 {
            return Err(TsrError::InvalidInput {
                reason: "invalid TATR image dimensions".into(),
            });
        }
        let rgb = image::RgbImage::from_raw(
            image.width(),
            image.height(),
            image.data().to_vec(),
        )
        .ok_or_else(|| TsrError::InvalidInput {
            reason: "TATR requires packed RGB pixels".into(),
        })?;
        let resized = image::imageops::resize(
            &rgb,
            width,
            height,
            image::imageops::FilterType::Triangle,
        );
        let mut pixels = Array4::zeros((1, 3, height as usize, width as usize));
        for (channel, (mean, deviation)) in
            [(0.485_f32, 0.229_f32), (0.456, 0.224), (0.406, 0.225)]
                .into_iter()
                .enumerate()
        {
            for (x, y, pixel) in resized.enumerate_pixels() {
                if let (Some(target), Some(value)) = (
                    pixels.get_mut((0, channel, y as usize, x as usize)),
                    pixel.0.get(channel),
                ) {
                    *target = (f32::from(*value) / 255.0 - mean) / deviation;
                }
            }
        }
        Ok(Self {
            pixels,
            mask: Array3::ones((1, height as usize, width as usize)),
        })
    }
}

impl TatrInput {
    /// Pads only to the largest ready crop and masks padding, preserving mixed-aspect batches.
    pub(crate) fn batch(inputs: &[&Self]) -> Result<Self, TsrError> {
        if inputs.is_empty()
            || inputs.len() > 32
            || inputs.iter().any(|input| {
                let (batch, channels, height, width) = input.pixels.dim();
                batch != 1
                    || channels != 3
                    || height == 0
                    || width == 0
                    || height > 800
                    || width > 800
                    || input.mask.dim() != (1, height, width)
            })
        {
            return Err(TsrError::InvalidInput {
                reason: "invalid TATR singleton batch".into(),
            });
        }
        let height = inputs
            .iter()
            .map(|input| input.pixels.dim().2)
            .max()
            .unwrap_or(0);
        let width = inputs
            .iter()
            .map(|input| input.pixels.dim().3)
            .max()
            .unwrap_or(0);
        let mut pixels = Array4::zeros((inputs.len(), 3, height, width));
        let mut mask = Array3::zeros((inputs.len(), height, width));
        for (index, input) in inputs.iter().enumerate() {
            let (_, _, h, w) = input.pixels.dim();
            pixels
                .slice_mut(ndarray::s![index..index + 1, .., ..h, ..w])
                .assign(&input.pixels);
            mask.slice_mut(ndarray::s![index..index + 1, ..h, ..w])
                .assign(&input.mask);
        }
        Ok(Self { pixels, mask })
    }
}

/// Copied singleton tensors cannot retain a session-owned buffer.
pub(crate) struct TatrOutput {
    logits: Array2<f32>,
    boxes: Array2<f32>,
}

impl TatrOutput {
    /// Enforces the pinned 125-query, six-class-plus-background contract before splitting a batch.
    pub(crate) fn from_batch(
        outputs: &SessionOutputs<'_>,
        batch: usize,
    ) -> Result<Vec<Self>, TsrError> {
        let extract = |name| -> Result<_, TsrError> {
            outputs
                .get(name)
                .ok_or_else(|| TsrError::InvalidInput {
                    reason: format!("missing TATR {name}"),
                })?
                .try_extract_array::<f32>()?
                .into_dimensionality::<Ix3>()
                .map_err(|error| TsrError::InvalidInput {
                    reason: error.to_string(),
                })
        };
        let logits = extract("logits")?;
        let boxes = extract("pred_boxes")?;
        if !(1..=32).contains(&batch)
            || logits.dim() != (batch, 125, 7)
            || boxes.dim() != (batch, 125, 4)
            || logits
                .iter()
                .chain(boxes.iter())
                .any(|value| !value.is_finite())
        {
            return Err(TsrError::InvalidInput {
                reason:
                    "expected finite TATR [B,125,7] logits and [B,125,4] boxes"
                        .into(),
            });
        }
        Ok(logits
            .axis_iter(Axis(0))
            .zip(boxes.axis_iter(Axis(0)))
            .map(|(logits, boxes)| Self {
                logits: logits.to_owned(),
                boxes: boxes.to_owned(),
            })
            .collect())
    }

    /// Converts accepted object queries to bounded crop coordinates and reconstructs row-major tokens.
    #[allow(
        clippy::indexing_slicing,
        reason = "tensor shapes are validated before splitting and rectangle arrays have four coordinates"
    )]
    pub(crate) fn decode(
        self,
        width: u32,
        height: u32,
    ) -> Result<TsrPrediction, TsrError> {
        let mut objects = Vec::new();
        for (logits, bbox) in
            self.logits.outer_iter().zip(self.boxes.outer_iter())
        {
            let Some((class, &maximum)) =
                logits.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))
            else {
                continue;
            };
            let score = 1.0
                / logits
                    .iter()
                    .map(|value| (value - maximum).exp())
                    .sum::<f32>();
            if class == 6 || score < 0.5 {
                continue;
            }
            let [cx, cy, w, h] =
                [bbox[0], bbox[1], bbox[2], bbox[3]].map(f64::from);
            let bbox = [
                ((cx - w / 2.0) * f64::from(width))
                    .clamp(0.0, f64::from(width)),
                ((cy - h / 2.0) * f64::from(height))
                    .clamp(0.0, f64::from(height)),
                ((cx + w / 2.0) * f64::from(width))
                    .clamp(0.0, f64::from(width)),
                ((cy + h / 2.0) * f64::from(height))
                    .clamp(0.0, f64::from(height)),
            ];
            if bbox[2] > bbox[0] && bbox[3] > bbox[1] {
                objects.push(Object { class, score, bbox });
            }
        }
        TsrPrediction::from_tatr(objects)
    }
}

/// A classified crop-space rectangle; class IDs follow the pinned Microsoft checkpoint.
#[derive(Clone)]
struct Object {
    class: usize,
    score: f32,
    bbox: [f64; 4],
}

impl Object {
    /// Measures coverage of the other rectangle, matching geometry-only TATR suppression.
    #[allow(
        clippy::indexing_slicing,
        reason = "rectangle coordinates use fixed four-element arrays"
    )]
    fn coverage(&self, other: &Self) -> f64 {
        let overlap = (self.bbox[2].min(other.bbox[2])
            - self.bbox[0].max(other.bbox[0]))
        .max(0.0)
            * (self.bbox[3].min(other.bbox[3])
                - self.bbox[1].max(other.bbox[1]))
            .max(0.0);
        overlap
            / ((other.bbox[2] - other.bbox[0])
                * (other.bbox[3] - other.bbox[1]))
    }

    /// Suppresses duplicate rows or columns in score order, then restores spatial order.
    #[allow(
        clippy::indexing_slicing,
        reason = "axis is selected internally as zero or one"
    )]
    fn axes(objects: &[Self], class: usize, axis: usize) -> Vec<Self> {
        let mut candidates: Vec<_> = objects
            .iter()
            .filter(|object| object.class == class)
            .cloned()
            .collect();
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut kept: Vec<Self> = Vec::new();
        for candidate in candidates {
            if kept.iter().all(|object| object.coverage(&candidate) < 0.5) {
                kept.push(candidate);
            }
        }
        kept.sort_by(|a, b| {
            (a.bbox[axis] + a.bbox[axis + 2])
                .total_cmp(&(b.bbox[axis] + b.bbox[axis + 2]))
        });
        kept
    }

    /// Finds the contiguous grid interval covered by at least half of each row or column.
    #[allow(
        clippy::indexing_slicing,
        reason = "axis is selected internally as zero or one"
    )]
    fn interval(&self, axes: &[Self], axis: usize) -> Option<Range<usize>> {
        let mut hits = axes.iter().enumerate().filter_map(|(index, item)| {
            let overlap = self.bbox[axis + 2].min(item.bbox[axis + 2])
                - self.bbox[axis].max(item.bbox[axis]);
            (overlap >= (item.bbox[axis + 2] - item.bbox[axis]) * 0.5)
                .then_some(index)
        });
        let first = hits.next()?;
        Some(first..hits.next_back().unwrap_or(first) + 1)
    }
}

/// A rectangular merged-cell interval already aligned to the recovered grid.
struct Span {
    rows: Range<usize>,
    columns: Range<usize>,
}

impl Span {
    /// Shrinks the less confident proposal at grid edges, following Microsoft's span conflict rule.
    fn remove_overlap(&mut self, other: &Self) {
        while self.rows.start < other.rows.end
            && other.rows.start < self.rows.end
            && self.columns.start < other.columns.end
            && other.columns.start < self.columns.end
            && !self.rows.is_empty()
            && !self.columns.is_empty()
        {
            let (current, occupied) = if self.rows.len() < self.columns.len() {
                (&mut self.columns, &other.columns)
            } else {
                (&mut self.rows, &other.rows)
            };
            if occupied.contains(&(current.end - 1)) {
                current.end -= 1;
            } else if occupied.contains(&current.start) {
                current.start += 1;
            } else {
                current.end = current.start;
            }
        }
    }
}

impl TsrPrediction {
    /// Builds topology from rows/columns and confidence-ordered spans without requiring OCR text.
    #[allow(
        clippy::indexing_slicing,
        reason = "all grid indices originate in validated row/column ranges"
    )]
    fn from_tatr(mut objects: Vec<Object>) -> Result<Self, TsrError> {
        let Some(table) = objects
            .iter()
            .filter(|object| object.class == 0)
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .cloned()
        else {
            return Err(TsrError::InvalidInput {
                reason: "TATR found no table".into(),
            });
        };
        objects.retain(|object| table.coverage(object) >= 0.5);
        let rows = Object::axes(&objects, 2, 1);
        let columns = Object::axes(&objects, 1, 0);
        if rows.is_empty() || columns.is_empty() {
            return Err(TsrError::InvalidInput {
                reason: "TATR found no row/column grid".into(),
            });
        }
        // Merge adjacent/overlapping header detections before defining the header/body boundary.
        let mut headers: Vec<_> = objects
            .iter()
            .filter(|object| object.class == 3)
            .filter_map(|object| object.interval(&rows, 1))
            .collect();
        headers.sort_by_key(|range| range.start);
        let mut header_end = 0;
        for (index, header) in headers.into_iter().enumerate() {
            if index > 0 && header.start > header_end {
                break;
            }
            header_end = header_end.max(header.end);
        }
        objects.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut spans: Vec<Span> = Vec::new();
        for object in objects
            .iter()
            .filter(|object| matches!(object.class, 4 | 5))
        {
            let (Some(mut r), Some(c)) =
                (object.interval(&rows, 1), object.interval(&columns, 0))
            else {
                continue;
            };
            // Merged cells cannot cross the header/body boundary; keep the larger group (header on ties).
            if r.start < header_end && r.end > header_end {
                if r.end - header_end > header_end - r.start {
                    r.start = header_end;
                } else {
                    r.end = header_end;
                }
            }
            if r.len() * c.len() < 2 {
                continue;
            }
            let mut span = Span {
                rows: r,
                columns: c,
            };
            for previous in &spans {
                span.remove_overlap(previous);
            }
            if span.rows.len() * span.columns.len() >= 2 {
                spans.push(span);
            }
        }
        // Header merges form a tree: each lower merge needs exactly one containing ancestor per earlier row.
        let rejected: Vec<_> = spans
            .iter()
            .map(|span| {
                span.rows.end <= header_end
                    && (0..span.rows.start).any(|row| {
                        spans
                            .iter()
                            .filter(|parent| {
                                parent.rows.end <= span.rows.start
                                    && parent.rows.contains(&row)
                                    && parent.columns.start
                                        <= span.columns.start
                                    && parent.columns.end >= span.columns.end
                            })
                            .count()
                            != 1
                    })
            })
            .collect();
        let mut index = 0;
        spans.retain(|_| {
            let keep = !rejected[index];
            index += 1;
            keep
        });
        let mut tokens: Vec<String> =
            ["<html>", "<body>", "<table>"].map(str::to_owned).to_vec();
        let mut boxes = Vec::new();
        for row in 0..rows.len() {
            if row == 0 {
                tokens.push(
                    if header_end > 0 { "<thead>" } else { "<tbody>" }.into(),
                );
            }
            if row == header_end && row > 0 {
                tokens.extend(["</thead>".into(), "<tbody>".into()]);
            }
            tokens.push("<tr>".into());
            for col in 0..columns.len() {
                let span = spans.iter().find(|span| {
                    span.rows.contains(&row) && span.columns.contains(&col)
                });
                if span.is_some_and(|span| {
                    span.rows.start != row || span.columns.start != col
                }) {
                    continue;
                }
                let (end_row, end_col) = span
                    .map_or((row + 1, col + 1), |span| {
                        (span.rows.end, span.columns.end)
                    });
                if span.is_some() {
                    tokens.push("<td".into());
                    if end_row - row > 1 {
                        tokens.push(format!(" rowspan=\"{}\"", end_row - row));
                    }
                    if end_col - col > 1 {
                        tokens.push(format!(" colspan=\"{}\"", end_col - col));
                    }
                    tokens.extend([">".into(), "</td>".into()]);
                } else {
                    tokens.push("<td></td>".into());
                }
                boxes.push(vec![
                    columns[col].bbox[0],
                    rows[row].bbox[1],
                    columns[end_col - 1].bbox[2],
                    rows[end_row - 1].bbox[3],
                ]);
            }
            tokens.push("</tr>".into());
        }
        tokens.push(
            if header_end == rows.len() {
                "</thead>"
            } else {
                "</tbody>"
            }
            .into(),
        );
        tokens.extend(["</table>".into(), "</body>".into(), "</html>".into()]);
        // Use model confidence; Microsoft's text-coverage score is undefined without OCR tokens.
        let score = rows
            .iter()
            .chain(&columns)
            .map(|object| f64::from(object.score))
            .sum::<f64>()
            / (rows.len() + columns.len()) as f64;
        Ok(Self::builder()
            .structure_tokens(tokens)
            .cell_bboxes(boxes)
            .score(score)
            .build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RGB normalization must preserve channel order, valid dimensions and an all-valid singleton mask.
    #[test]
    fn preprocessing_preserves_rgb_and_aspect_ratio() {
        let image = PageImage::try_from(
            docparse_layout::PageImageInput::builder()
                .width(2)
                .height(1)
                .pixel_format(docparse_layout::PixelFormat::Rgb8)
                .data(std::sync::Arc::from([255_u8, 0, 0, 255, 0, 0]))
                .build(),
        )
        .expect("RGB crop");
        let input = TatrInput::try_from(&image).expect("TATR input");
        assert_eq!(input.pixels.dim(), (1, 3, 400, 800));
        assert!(
            (input.pixels.get((0, 0, 0, 0)).expect("red")
                - (1.0 - 0.485) / 0.229)
                .abs()
                < 1e-5
        );
        assert!(
            (input.pixels.get((0, 2, 0, 0)).expect("blue") + 0.406 / 0.225)
                .abs()
                < 1e-5
        );
        assert!(input.mask.iter().all(|value| *value == 1));
    }

    /// Adjacent header detections describe one header prefix even when queries arrive out of order.
    #[test]
    fn adjacent_headers_keep_both_rows() {
        let mut objects = vec![
            Object {
                class: 0,
                score: 0.99,
                bbox: [0.0, 0.0, 40.0, 30.0],
            },
            Object {
                class: 1,
                score: 0.99,
                bbox: [0.0, 0.0, 40.0, 30.0],
            },
        ];
        objects.extend((0..3).map(|r| Object {
            class: 2,
            score: 0.99,
            bbox: [0.0, f64::from(r) * 10.0, 40.0, f64::from(r + 1) * 10.0],
        }));
        for r in [1, 0] {
            objects.push(Object {
                class: 3,
                score: 0.9,
                bbox: [0.0, f64::from(r) * 10.0, 40.0, f64::from(r + 1) * 10.0],
            });
        }
        let prediction =
            TsrPrediction::from_tatr(objects).expect("header grid");
        assert_eq!(
            prediction
                .structure_tokens
                .iter()
                .take_while(|t| t.as_str() != "</thead>")
                .filter(|t| t.as_str() == "<tr>")
                .count(),
            2
        );
    }

    /// Execute a physical batch of distinct shapes and compare each slice with its singleton result.
    #[test]
    #[ignore = "requires exported TATR ONNX"]
    fn real_mixed_batch_matches_singletons() {
        use crate::{
            artifacts::ModelKind, model::ModelResult, preprocess::ModelInput,
        };
        use ort::session::builder::SessionBuilder;
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let raster =
            image::open(root.join("crates/tsr/tests/fixtures/table.png"))
                .expect("fixture")
                .into_rgb8();
        let rotated = image::imageops::rotate90(&raster);
        let inputs: Vec<_> = [raster, rotated]
            .into_iter()
            .map(|raster| {
                let image = PageImage::try_from(
                    docparse_layout::PageImageInput::builder()
                        .width(raster.width())
                        .height(raster.height())
                        .pixel_format(docparse_layout::PixelFormat::Rgb8)
                        .data(std::sync::Arc::from(raster.into_raw()))
                        .build(),
                )
                .expect("image");
                ModelInput::Tatr(
                    TatrInput::try_from(&image).expect("preprocess"),
                )
            })
            .collect();
        let mut session =
            SessionBuilder::try_from(docparse_layout::OnnxBackend::compiled())
                .expect("backend")
                .commit_from_file(
                    root.join("models/tatr-v1.1-all/inference.onnx"),
                )
                .expect("session");
        let kind = ModelKind::Structure(docparse_config::TsrModel::Tatr);
        let mut singles = Vec::new();
        for input in &inputs {
            let outputs = session
                .run(input.values().expect("inputs"))
                .expect("singleton");
            singles.extend(
                ModelResult::from_batch(kind, 1, &outputs).expect("split"),
            );
        }
        let batch = ModelInput::batch(&inputs.iter().collect::<Vec<_>>())
            .expect("batch");
        let ModelInput::Tatr(ref tensor) = batch else {
            unreachable!("TATR")
        };
        assert_eq!(tensor.pixels.dim(), (2, 3, 800, 800));
        let outputs = session
            .run(batch.values().expect("inputs"))
            .expect("physical batch=2");
        let batched =
            ModelResult::from_batch(kind, 2, &outputs).expect("split batch");
        for (single, batch) in singles.into_iter().zip(batched) {
            let (ModelResult::Tatr(single), ModelResult::Tatr(batch)) =
                (single, batch)
            else {
                unreachable!("TATR")
            };
            // Padding changes convolution boundaries slightly; compare confident query geometry, not background logits.
            let mut matched = 0;
            for (index, logits) in single.logits.outer_iter().enumerate() {
                let (class, maximum) = logits
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .expect("classes");
                let probability = 1.0
                    / logits
                        .iter()
                        .map(|value| (value - maximum).exp())
                        .sum::<f32>();
                if class == 6 || probability < 0.9 {
                    continue;
                }
                let row = batch.logits.row(index);
                assert_eq!(
                    row.iter()
                        .enumerate()
                        .max_by(|a, b| a.1.total_cmp(b.1))
                        .expect("classes")
                        .0,
                    class
                );
                assert!(
                    single
                        .boxes
                        .row(index)
                        .iter()
                        .zip(batch.boxes.row(index))
                        .all(|(a, b)| (a - b).abs() < 0.03)
                );
                matched += 1;
            }
            assert!(matched > 0);
        }
    }

    /// Reports final topology and crop-coordinate drift for physical batches across aspect ratios.
    #[test]
    #[ignore = "requires exported TATR; emits an empirical batch consistency report"]
    fn real_batch_matrix_reports_topology() {
        use crate::{
            artifacts::ModelKind, model::ModelResult, preprocess::ModelInput,
        };
        use ort::session::builder::SessionBuilder;
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let raster =
            image::open(root.join("crates/tsr/tests/fixtures/table.png"))
                .expect("fixture")
                .into_rgb8();
        let mut padded =
            image::RgbImage::from_pixel(900, 900, image::Rgb([255, 255, 255]));
        image::imageops::replace(&mut padded, &raster, 30, 100);
        let cases = [
            raster.clone(),
            image::imageops::resize(
                &raster,
                400,
                800,
                image::imageops::FilterType::Triangle,
            ),
            image::imageops::resize(
                &raster,
                1200,
                200,
                image::imageops::FilterType::Triangle,
            ),
            padded,
        ];
        let dimensions: Vec<_> =
            cases.iter().map(image::RgbImage::dimensions).collect();
        let inputs: Vec<_> = cases
            .into_iter()
            .map(|raster| {
                let image = PageImage::try_from(
                    docparse_layout::PageImageInput::builder()
                        .width(raster.width())
                        .height(raster.height())
                        .pixel_format(docparse_layout::PixelFormat::Rgb8)
                        .data(std::sync::Arc::from(raster.into_raw()))
                        .build(),
                )
                .expect("image");
                ModelInput::Tatr(
                    TatrInput::try_from(&image).expect("preprocess"),
                )
            })
            .collect();
        let backend = docparse_layout::OnnxBackend::compiled();
        let mut session = SessionBuilder::try_from(backend)
            .expect("backend")
            .commit_from_file(root.join("models/tatr-v1.1-all/inference.onnx"))
            .expect("session");
        let kind = ModelKind::Structure(docparse_config::TsrModel::Tatr);
        let mut singles = Vec::new();
        for (input, &(width, height)) in inputs.iter().zip(&dimensions) {
            let outputs = session
                .run(input.values().expect("inputs"))
                .expect("singleton");
            let ModelResult::Tatr(single) =
                ModelResult::from_batch(kind, 1, &outputs)
                    .expect("split")
                    .remove(0)
            else {
                unreachable!("TATR")
            };
            singles.push(single.decode(width, height).expect("singleton grid"));
        }
        let mut report = Vec::new();
        for size in [1, 2, 4, 8] {
            for homogeneous in [false, true] {
                for reverse in [false, true] {
                    let indices: Vec<_> = (0..size)
                        .map(|index| {
                            if homogeneous {
                                usize::from(reverse)
                            } else if reverse {
                                3 - index % 4
                            } else {
                                index % 4
                            }
                        })
                        .collect();
                    let selected: Vec<_> = indices
                        .iter()
                        .map(|index| inputs.get(*index).expect("input"))
                        .collect();
                    let batch = ModelInput::batch(&selected).expect("batch");
                    let outputs = session
                        .run(batch.values().expect("inputs"))
                        .expect("physical batch");
                    let results = ModelResult::from_batch(kind, size, &outputs)
                        .expect("split");
                    assert_eq!(results.len(), size);
                    for (position, (index, result)) in
                        indices.iter().zip(results).enumerate()
                    {
                        let ModelResult::Tatr(result) = result else {
                            unreachable!("TATR")
                        };
                        let &(width, height) =
                            dimensions.get(*index).expect("dimensions");
                        let reference = singles.get(*index).expect("reference");
                        match result.decode(width,height) {
                        Ok(prediction) => {
                            let same = reference.structure_tokens == prediction.structure_tokens;
                            let delta = same.then(|| reference.cell_bboxes.iter().flatten().zip(prediction.cell_bboxes.iter().flatten())
                                .map(|(a,b)| (a-b).abs()).fold(0.0_f64,f64::max));
                            report.push(serde_json::json!({"homogeneous":homogeneous,"batch":size,"reverse":reverse,"position":position,"sample":index,
                                "reference_cells":reference.cell_bboxes.len(),"batch_cells":prediction.cell_bboxes.len(),
                                "same_topology":same,"max_box_delta_pixels":delta}));
                        }
                        Err(error) => report.push(serde_json::json!({"homogeneous":homogeneous,"batch":size,"sample":index,"error":error.to_string()})),
                    }
                    }
                }
            }
        }
        let path = root.join(format!(
            "models/tatr-v1.1-all/batch-{}.json",
            backend.execution_provider()
        ));
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&report).expect("report"),
        )
        .expect("save report");
        println!("batch report: {}", path.display());
        println!("{}", serde_json::to_string(&report).expect("report"));
    }

    /// Mixed-aspect batches retain sample order and mask only padded pixels.
    #[test]
    fn mixed_shapes_keep_masks_and_reject_invalid_samples() {
        let first = TatrInput {
            pixels: Array4::from_elem((1, 3, 2, 4), 1.0),
            mask: Array3::ones((1, 2, 4)),
        };
        let second = TatrInput {
            pixels: Array4::from_elem((1, 3, 4, 2), 2.0),
            mask: Array3::ones((1, 4, 2)),
        };
        let batch = TatrInput::batch(&[&first, &second]).expect("padded batch");
        assert_eq!(batch.pixels.dim(), (2, 3, 4, 4));
        assert_eq!(batch.pixels.get((1, 0, 3, 1)), Some(&2.0));
        assert_eq!(batch.pixels.get((0, 0, 3, 1)), Some(&0.0));
        assert_eq!(batch.mask.get((0, 3, 1)), Some(&0));
        assert_eq!(batch.mask.get((1, 3, 1)), Some(&1));
        assert!(TatrInput::batch(&[]).is_err());
        let invalid = TatrInput {
            pixels: Array4::zeros((2, 3, 2, 4)),
            mask: Array3::ones((1, 2, 4)),
        };
        assert!(TatrInput::batch(&[&invalid]).is_err());
    }

    /// Overlapping span proposals keep valid residual merges instead of silently losing them.
    #[test]
    fn conflicting_spans_shrink_to_disjoint_rectangles() {
        let mut objects = vec![
            Object {
                class: 0,
                score: 0.99,
                bbox: [0.0, 0.0, 40.0, 10.0],
            },
            Object {
                class: 2,
                score: 0.99,
                bbox: [0.0, 0.0, 40.0, 10.0],
            },
            Object {
                class: 5,
                score: 0.95,
                bbox: [0.0, 0.0, 20.0, 10.0],
            },
            Object {
                class: 5,
                score: 0.9,
                bbox: [10.0, 0.0, 40.0, 10.0],
            },
        ];
        objects.extend((0..4).map(|index| Object {
            class: 1,
            score: 0.99,
            bbox: [
                f64::from(index) * 10.0,
                0.0,
                f64::from(index + 1) * 10.0,
                10.0,
            ],
        }));
        let prediction = TsrPrediction::from_tatr(objects).expect("grid");
        assert_eq!(
            prediction.cell_bboxes,
            vec![vec![0.0, 0.0, 20.0, 10.0], vec![20.0, 0.0, 40.0, 10.0]]
        );
        assert_eq!(
            prediction
                .structure_tokens
                .iter()
                .filter(|token| token.as_str() == " colspan=\"2\"")
                .count(),
            2
        );
        TsrPrediction::from_tatr(Vec::new()).expect_err("empty detections");
    }
}
