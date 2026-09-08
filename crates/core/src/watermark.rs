use crate::line::{
    ConservativeLineAssembler, LineAssembler, LineFragment, TextAxes,
};
use crate::{Evidence, ExtractedPage, LineError, TextSource, WatermarkSource};
use docparse_config::FusionConfig;
use docparse_layout::Polygon;
use std::collections::{BTreeMap, BTreeSet};

impl LineFragment {
    /// Uses content and direction for repetition evidence without hard-coded watermark wording.
    fn watermark_fingerprint(&self) -> String {
        let text: String = self
            .items
            .iter()
            .flat_map(|item| item.raw_text.chars())
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect();
        format!("{}:{text}", (self.rotation.rem_euclid(180.0) / 5.0).round())
    }
}

/// Applies conservative text rules only after PDFium has supplied authoritative content marks.
pub(crate) fn classify<'a>(
    pages: impl IntoIterator<Item = &'a mut ExtractedPage>,
    config: &FusionConfig,
) -> Result<(), LineError> {
    let mut pages: Vec<_> = pages.into_iter().collect();
    // Figure-only pages may contain only the large overlay. Ordinary orientations on
    // other pages provide a size reference without treating the overlay as body text.
    let mut document_fonts: Vec<_> = pages
        .iter()
        .flat_map(|page| &page.text_items)
        .filter(|item| {
            item.watermark.is_none()
                && !TextAxes::from(item.rotation).is_oblique()
        })
        .filter_map(|item| {
            item.style
                .as_ref()
                .and_then(|style| style.font_height.or(style.font_size))
        })
        .filter(|size| size.is_finite() && *size > 0.0)
        .collect();
    document_fonts.sort_by(f64::total_cmp);
    let document_body = document_fonts
        .get(document_fonts.len() / 2)
        .copied()
        .unwrap_or(10.0);
    let mut candidates = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for page in &pages {
        let mut fonts: Vec<_> = page
            .text_items
            .iter()
            .filter(|item| item.watermark.is_none())
            .filter_map(|item| {
                item.style
                    .as_ref()
                    .and_then(|style| style.font_height.or(style.font_size))
            })
            .filter(|value| value.is_finite() && *value > 0.0)
            .collect();
        fonts.sort_by(f64::total_cmp);
        let body = fonts
            .get(fonts.len() / 2)
            .copied()
            .unwrap_or(document_body)
            .min(document_body);
        let items = page
            .text_items
            .iter()
            .filter(|item| {
                if item.source != TextSource::Native || item.watermark.is_some()
                {
                    return false;
                }
                let large = item
                    .style
                    .as_ref()
                    .and_then(|style| style.font_height.or(style.font_size))
                    .is_some_and(|size| size >= body * 1.8);
                let transparent = item
                    .style
                    .as_ref()
                    .and_then(|style| style.fill_color)
                    .is_some_and(|color| color[3] < 192);
                // Reconstruct the line before testing its page span: PDF producers may
                // emit each watermark word or glyph as a separate, individually narrow object.
                large
                    || TextAxes::from(item.rotation).is_oblique()
                    || transparent
            })
            .cloned()
            .collect();
        let lines = ConservativeLineAssembler.fragments(items, config)?;
        // Count pages rather than occurrences: tiled marks on one page do not prove repetition.
        for key in lines
            .iter()
            .map(LineFragment::watermark_fingerprint)
            .collect::<BTreeSet<_>>()
        {
            *counts.entry(key).or_default() += 1;
        }
        candidates.push((body, lines));
    }
    let page_count = pages.len();
    for (page, (body, lines)) in pages.iter_mut().zip(candidates) {
        let mut confirmed = 0;
        for line in lines {
            let key = line.watermark_fingerprint();
            let repetitions = counts.get(&key).copied().unwrap_or(0);
            let repeated =
                repetitions >= 2 && repetitions * 5 >= page_count * 3;
            let center = line.bbox.center();
            let interior = center.x > page.width * 0.1
                && center.x < page.width * 0.9
                && center.y > page.height * 0.15
                && center.y < page.height * 0.85;
            let wide = line.bbox.width().max(line.bbox.height())
                >= page.width.min(page.height) * 0.45;
            let transparent = line.items.iter().all(|item| {
                item.style
                    .as_ref()
                    .and_then(|style| style.fill_color)
                    .is_some_and(|color| color[3] < 192)
            });
            let oblique = TextAxes::from(line.rotation).is_oblique();
            let large = line
                .items
                .iter()
                .filter_map(|item| {
                    item.style
                        .as_ref()
                        .and_then(|style| style.font_height.or(style.font_size))
                })
                .fold(0.0_f64, f64::max)
                >= body * 1.8;
            let ids: BTreeSet<_> =
                line.items.iter().map(|item| item.id.clone()).collect();
            let shape = Polygon::enclosing(
                line.items
                    .iter()
                    .filter_map(|item| item.polygon.as_ref())
                    .flat_map(|polygon| polygon.points().iter().copied()),
            )
            .ok();
            let crossings = page
                .text_items
                .iter()
                .filter(|item| {
                    item.watermark.is_none()
                        && !ids.contains(&item.id)
                        && item
                            .style
                            .as_ref()
                            .and_then(|style| {
                                style.font_height.or(style.font_size)
                            })
                            .is_some_and(|size| size <= body * 1.4)
                })
                .filter(|item| {
                    // AABB overlap alone includes the empty triangles beside diagonal text.
                    shape.as_ref().is_some_and(|polygon| {
                        polygon.intersection_area(item.bbox)
                            > item.bbox.area() * 0.05
                    })
                })
                .map(|item| (item.bbox.top / body.max(1.0)).round() as i64)
                .collect::<BTreeSet<_>>()
                .len();
            // Large, page-spanning repeated overlays also cover figure-only pages. A local
            // decision needs translucency plus several crossed body lines. Tilt alone never wins.
            if !interior
                || !wide
                || !((repeated
                    && (large || transparent)
                    && (oblique || transparent || (large && crossings >= 2)))
                    || (transparent && large && crossings >= 3))
            {
                continue;
            }
            let evidence = Evidence::builder()
                .kind("watermark_text_pattern".to_owned())
                .details(BTreeMap::from([
                    ("repeated_pages".to_owned(), repetitions.to_string()),
                    ("document_pages".to_owned(), page_count.to_string()),
                    ("crossed_body_lines".to_owned(), crossings.to_string()),
                    ("translucent".to_owned(), transparent.to_string()),
                    ("oblique".to_owned(), oblique.to_string()),
                ]))
                .build();
            for item in &mut page.text_items {
                if ids.contains(&item.id) {
                    item.watermark = Some(WatermarkSource::TextPattern);
                    page.watermark_evidence
                        .insert(item.id.clone(), evidence.clone());
                    confirmed += 1;
                }
            }
        }
        let explicit = page
            .text_items
            .iter()
            .filter(|item| {
                item.watermark == Some(WatermarkSource::PdfMarkedContent)
            })
            .count();
        if explicit + confirmed + page.watermark_annotations.len() > 0 {
            tracing::debug!(
                "identified {} PDF-marked text runs, {} watermark annotations, and {} rule-derived text runs on page {}",
                explicit,
                page.watermark_annotations.len(),
                confirmed,
                page.page_number
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        ExtractedPage, TextItem, TextItemId, TextSource, TextStyle,
        WatermarkSource,
    };
    use docparse_config::FusionConfig;
    use docparse_layout::{Bbox, Point, Polygon};

    /// Builds broad body lines, a large overlay, and a small tilted chart label.
    fn page(number: u32, horizontal: bool) -> ExtractedPage {
        let mut items = Vec::new();
        for (index, y) in [150.0, 230.0, 310.0, 390.0, 470.0, 550.0]
            .into_iter()
            .enumerate()
        {
            items.push(
                TextItem::builder()
                    .id(TextItemId::native(number, index as u32))
                    .raw_text("Ordinary body text".into())
                    .bbox(
                        Bbox::try_from([50.0, y, 550.0, y + 15.0])
                            .expect("body box"),
                    )
                    .source(TextSource::Native)
                    .style(Some(
                        TextStyle::builder().font_size(Some(10.0)).build(),
                    ))
                    .extraction_order(index as u32)
                    .build(),
            );
        }
        let points = if horizontal {
            vec![
                Point::new(100.0, 300.0),
                Point::new(500.0, 300.0),
                Point::new(500.0, 420.0),
                Point::new(100.0, 420.0),
            ]
        } else {
            vec![
                Point::new(50.0, 680.0),
                Point::new(500.0, 230.0),
                Point::new(532.0, 262.0),
                Point::new(82.0, 712.0),
            ]
        };
        let polygon = Polygon::try_from(points).expect("overlay polygon");
        items.push(
            TextItem::builder()
                .id(TextItemId::native(number, 6))
                .raw_text("CONFIDENTIAL".into())
                .bbox(polygon.bbox())
                .polygon(Some(polygon))
                .rotation(if horizontal { 0.0 } else { 315.0 })
                .source(TextSource::Native)
                .style(Some(
                    TextStyle::builder()
                        .font_size(Some(48.0))
                        .fill_color(Some([120, 120, 120, 255]))
                        .build(),
                ))
                .extraction_order(6)
                .build(),
        );
        items.push(
            TextItem::builder()
                .id(TextItemId::native(number, 7))
                .raw_text("Chart axis".into())
                .bbox(
                    Bbox::try_from([80.0, 150.0, 160.0, 230.0])
                        .expect("chart label"),
                )
                .rotation(315.0)
                .source(TextSource::Native)
                .style(Some(TextStyle::builder().font_size(Some(12.0)).build()))
                .extraction_order(7)
                .build(),
        );
        ExtractedPage::builder()
            .page_number(number)
            .width(600.0)
            .height(800.0)
            .rotation(0)
            .text_items(items)
            .build()
    }

    /// Repetition plus overlay geometry supports both orientations without relabeling chart text.
    #[test]
    fn repeated_overlay_is_independent_of_orientation() {
        for horizontal in [false, true] {
            let mut pages = [page(1, horizontal), page(2, horizontal)];
            super::classify(pages.iter_mut(), &FusionConfig::default())
                .expect("classify");
            for page in pages {
                assert_eq!(
                    page.text_items.get(6).expect("fixture item").watermark,
                    Some(WatermarkSource::TextPattern)
                );
                assert!(
                    page.text_items
                        .get(..6)
                        .expect("body items")
                        .iter()
                        .all(|item| item.watermark.is_none())
                );
                assert_eq!(
                    page.text_items.get(7).expect("fixture item").watermark,
                    None
                );
            }
        }
    }

    /// Repeated diagonal chart text still needs overlay paint or large typography evidence.
    #[test]
    fn repeated_wide_chart_label_is_not_a_watermark() {
        let mut pages = [page(1, false), page(2, false)];
        for page in &mut pages {
            let item = page.text_items.get_mut(7).expect("chart label");
            item.bbox = Bbox::try_from([80.0, 200.0, 500.0, 620.0])
                .expect("wide chart label");
        }
        super::classify(pages.iter_mut(), &FusionConfig::default())
            .expect("classify");
        for page in pages {
            assert_eq!(
                page.text_items.get(7).expect("chart label").watermark,
                None
            );
        }
    }

    /// A single opaque tilted title has insufficient evidence; an explicit PDF mark takes priority.
    #[test]
    fn pdf_marks_take_priority_without_guessing_from_tilt() {
        let mut page = page(1, false);
        page.text_items.first_mut().expect("first item").watermark =
            Some(WatermarkSource::PdfMarkedContent);
        super::classify(std::iter::once(&mut page), &FusionConfig::default())
            .expect("classify");
        assert_eq!(
            page.text_items.first().expect("fixture item").watermark,
            Some(WatermarkSource::PdfMarkedContent)
        );
        assert_eq!(
            page.text_items.get(6).expect("fixture item").watermark,
            None
        );
    }
}
