use docparse_config::{OcrConfig, OcrPolicy};
use docparse_layout::{Bbox, Point};
use typed_builder::TypedBuilder;

use super::{index::TextIndex, spacing::space_words};
use crate::page::OcrCompletion;
use crate::{
    Baseline, PageWarning, TextItem, TextItemId, TextSource, TextStyle,
};

/// Merges visual text against a fixed native snapshot before semantic ownership is assigned.
#[derive(TypedBuilder)]
pub(crate) struct OcrMerger<'a> {
    pub(crate) config: &'a OcrConfig,
    pub(crate) page_number: u32,
    pub(crate) page_bbox: Bbox,
    pub(crate) missing_regions: &'a [Bbox],
}

impl OcrMerger<'_> {
    /// Keeps healthy PDF facts, archives reliable replacements, and reports failures without losing text.
    pub(crate) fn merge(
        &self,
        text_items: &mut Vec<TextItem>,
        warnings: &mut Vec<PageWarning>,
        completion: OcrCompletion,
    ) -> Vec<TextItem> {
        if self.config.policy == OcrPolicy::Disabled
            || self.missing_regions.is_empty()
        {
            return Vec::new();
        }
        let result = match completion {
            OcrCompletion::Succeeded(result) => result,
            OcrCompletion::Unavailable | OcrCompletion::NotRequested => {
                warnings.push(PageWarning {
                    code: "OcrUnavailable".into(),
                    stage: "ocr".into(),
                    message: "OCR was requested but no engine was available"
                        .into(),
                });
                return Vec::new();
            }
            OcrCompletion::Failed(message) => {
                warnings.push(PageWarning {
                    code: "OcrFailed".into(),
                    stage: "ocr".into(),
                    message,
                });
                return Vec::new();
            }
        };
        let unusable: Vec<_> =
            text_items.iter().map(TextItem::needs_ocr).collect();
        let healthy: TextIndex<'_> = text_items
            .iter()
            .zip(&unusable)
            .filter(|(item, invalid)| {
                item.source == TextSource::Native
                    && !**invalid
                    && !item.raw_text.trim().is_empty()
            })
            .map(|(item, _)| item)
            .collect();
        let mut accepted = Vec::new();
        // Query the immutable native index; accepted OCR must never suppress another OCR line.
        for (source_index, fact) in result.items.into_iter().enumerate() {
            let valid = u32::try_from(source_index).is_ok()
                && !fact.text.trim().is_empty()
                && fact.confidence.is_finite()
                && (0.0..=1.0).contains(&fact.confidence)
                && Bbox::try_from([
                    fact.bbox.left,
                    fact.bbox.top,
                    fact.bbox.right,
                    fact.bbox.bottom,
                ])
                .is_ok()
                && self.page_bbox.contains_bbox(fact.bbox)
                && fact
                    .polygon
                    .as_ref()
                    .is_none_or(|p| fact.bbox.contains_bbox(p.bbox()));
            if !valid {
                warnings.push(PageWarning {
                    code: "InvalidOcrResult".into(),
                    stage: "ocr".into(),
                    message: format!("ignored OCR result {source_index}"),
                });
                continue;
            }
            if fact.confidence <= 0.1
                || fact.confidence < self.config.recognition_threshold
                || !self.missing_regions.iter().any(|region| {
                    region.intersection_area(fact.bbox)
                        / fact.bbox.area().min(region.area())
                        >= 0.5
                })
            {
                continue;
            }
            if !text_items.is_empty()
                && fact.bbox.width() > fact.bbox.height() * 10.0
                && fact
                    .text
                    .trim()
                    .chars()
                    .all(|c| matches!(c, '|' | '_' | '─' | '—'))
            {
                continue;
            }
            let (baseline, rotation, font_height) = if let Some(points) =
                fact.polygon.as_ref().map(|p| p.points())
                && let [a, b, c, d] = points
            {
                // The recognizer's corner order includes crop rotation and 180-degree correction.
                (
                    Baseline { start: *d, end: *c },
                    (b.y - a.y).atan2(b.x - a.x).to_degrees().rem_euclid(360.0),
                    ((d.x - a.x).hypot(d.y - a.y)
                        + (c.x - b.x).hypot(c.y - b.y))
                        * 0.5,
                )
            } else {
                (
                    Baseline {
                        start: Point::new(fact.bbox.left, fact.bbox.bottom),
                        end: Point::new(fact.bbox.right, fact.bbox.bottom),
                    },
                    0.0,
                    fact.bbox.height(),
                )
            };
            accepted.extend(
                TextItem::builder()
                    .id(TextItemId::ocr(self.page_number, source_index as u32))
                    .raw_text(fact.text.trim().to_owned())
                    .raw_bbox(Some(fact.bbox))
                    .bbox(fact.bbox)
                    .polygon(fact.polygon)
                    .baseline(Some(baseline))
                    .rotation(rotation)
                    .source(TextSource::Ocr)
                    .confidence(Some(fact.confidence))
                    .extraction_order(source_index as u32)
                    .style(Some(
                        TextStyle::builder()
                            .font_name(Some("OCR".into()))
                            .font_size(Some(font_height))
                            .font_height(Some(font_height))
                            .font_size_estimated(true)
                            .build(),
                    ))
                    .build()
                    .without_native(&healthy, warnings),
            );
        }
        // Replacement is a page-level decision: split detector lines jointly cover native spans,
        // and a repeated box must never inflate the amount of reliable replacement evidence.
        let replaced: Vec<_> = {
            let reliable: TextIndex<'_> = accepted
                .iter()
                .filter(|item| {
                    item.confidence.is_some_and(|score| score >= 0.8)
                })
                .collect();
            text_items
                .iter()
                .zip(&unusable)
                .map(|(native, invalid)| {
                    *invalid
                        && native.bbox.covered_area(
                            reliable
                                .overlapping(native.bbox)
                                .map(|item| item.bbox),
                        ) / native.bbox.area()
                            >= 0.8
                })
                .collect()
        };
        // Query only unreplaced invalid facts, rather than scanning every healthy native item again.
        let unreplaced: TextIndex<'_> = text_items
            .iter()
            .zip(&unusable)
            .zip(&replaced)
            .filter(|((_, invalid), replaced)| **invalid && !**replaced)
            .map(|((item, _), _)| item)
            .collect();
        accepted.retain(|item| {
            !unreplaced.overlapping(item.bbox).any(|native| {
                native.bbox.intersection_area(item.bbox)
                    / native.bbox.area().min(item.bbox.area())
                    >= 0.5
            })
        });
        space_words(&mut accepted);
        let (archived, retained): (Vec<_>, Vec<_>) = std::mem::take(text_items)
            .into_iter()
            .zip(replaced)
            .partition(|(_, replaced)| *replaced);
        text_items.extend(retained.into_iter().map(|(item, _)| item));
        text_items.extend(accepted);
        archived.into_iter().map(|(item, _)| item).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measures spacing and full native/OCR merging with deterministic dense-page facts, without model startup noise.
    #[test]
    #[ignore = "release-mode microbenchmark; run ocr_perf_ with --ignored --nocapture --test-threads=1"]
    fn ocr_perf_text_merge() {
        let source: Vec<_> = (0..1000)
            .map(|index| {
                TextItem::builder()
                    .id(TextItemId::ocr(1, index))
                    .raw_text("example".into())
                    .bbox(
                        Bbox::try_from([
                            f64::from(index % 20) * 48.0,
                            f64::from(index / 20) * 18.0,
                            f64::from(index % 20) * 48.0 + 36.0,
                            f64::from(index / 20) * 18.0 + 12.0,
                        ])
                        .expect("box"),
                    )
                    .source(TextSource::Ocr)
                    .build()
            })
            .collect();
        let mut native = source.clone();
        for (index, item) in native.iter_mut().enumerate() {
            item.id = TextItemId::native(1, index as u32);
            item.source = TextSource::Native;
        }
        let facts = crate::OcrResult::builder()
            .items(
                source
                    .iter()
                    .map(|item| {
                        crate::OcrTextItem::builder()
                            .text(item.raw_text.clone())
                            .bbox(item.bbox)
                            .confidence(0.99)
                            .build()
                    })
                    .collect(),
            )
            .build();
        let mut raw = docparse_config::RawConfig::default();
        raw.ocr.policy = OcrPolicy::Always;
        let config = raw.ocr;
        let bounds = Bbox::try_from([0.0, 0.0, 1000.0, 1000.0]).expect("page");
        let merger = OcrMerger::builder()
            .config(&config)
            .page_number(1)
            .page_bbox(bounds)
            .missing_regions(std::slice::from_ref(&bounds))
            .build();
        for merge in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..7 {
                let start = std::time::Instant::now();
                for _ in 0..5 {
                    if merge {
                        let mut items = native.clone();
                        let mut warnings = Vec::new();
                        std::hint::black_box(merger.merge(
                            &mut items,
                            &mut warnings,
                            OcrCompletion::Succeeded(facts.clone()),
                        ));
                        assert_eq!(items.len(), native.len());
                        assert!(warnings.is_empty());
                        std::hint::black_box(items);
                    } else {
                        let mut items = source.clone();
                        space_words(&mut items);
                        std::hint::black_box(items);
                    }
                }
                samples.push(start.elapsed().as_secs_f64() * 1_000_000.0 / 5.0);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "ocr_perf {}: {:.3} us/page",
                if merge { "merge" } else { "spacing" },
                samples.get(3).expect("median")
            );
        }
    }
}
