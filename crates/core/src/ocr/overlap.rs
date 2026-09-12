use std::ops::Range;

use docparse_layout::{Bbox, Point, Polygon};

use super::index::TextIndex;
use crate::line::{TextAxes, bidi::detect_direction};
use crate::{Baseline, PageWarning, RepairAction, TextItem, WritingDirection};

/// Native-owned OCR bytes, their reading-axis interval, and the trusted source geometry.
struct NativeMatch<'a> {
    bytes: Range<usize>,
    inline: (f64, f64),
    item: &'a TextItem,
}

impl TextItem {
    /// Keeps only OCR text not already owned by geometrically corresponding native spans.
    pub(super) fn without_native(
        self,
        native: &TextIndex<'_>,
        warnings: &mut Vec<PageWarning>,
    ) -> Vec<Self> {
        let native: Vec<_> = native
            .overlapping(self.bbox)
            .filter(|item| {
                // Candidate filtering is a scalar check, not a union or allocation.
                let intersection = self.bbox.intersection_area(item.bbox);
                intersection / self.bbox.area().min(item.bbox.area()) >= 0.5
            })
            .collect();
        if native.is_empty() {
            return vec![self];
        }
        // Measure coverage of the OCR result itself: a small embedded label cannot own its entire line.
        if self.bbox.covered_area(native.iter().map(|item| item.bbox))
            / self.bbox.area()
            >= 0.8
        {
            return Vec::new();
        }
        let axes = TextAxes::from(self.rotation);
        let Ok(frame) = axes.project_bbox(self.bbox) else {
            return vec![self];
        };
        let rtl = detect_direction(std::slice::from_ref(&self), self.rotation)
            == WritingDirection::RightToLeft;
        let mut covered = self.native_matches(native, axes, frame, rtl);
        if covered.is_empty() {
            // An unmatched partial overlap is ambiguous; retain its value instead of guessing away characters.
            warnings.push(PageWarning {
                code: "OcrPartialOverlap".into(), stage: "ocr".into(),
                message: format!("retained {} because partial native overlap could not be aligned", self.id.as_str()),
            });
            return vec![self];
        }
        // Matched native font/baseline measurements remove detector padding from row grouping hints.
        let size = self
            .style
            .as_ref()
            .and_then(|style| style.font_size)
            .unwrap_or(self.bbox.height());
        let anchor = covered
            .iter()
            .filter_map(|matched| {
                let native = matched.item;
                let baseline = native.baseline?;
                let style = native
                    .style
                    .as_ref()
                    .filter(|style| !style.font_size_estimated)?;
                let font = style
                    .font_size
                    .filter(|font| font.is_finite() && *font > 0.0)?;
                let angle =
                    (native.rotation - self.rotation).abs().rem_euclid(360.0);
                ((0.5..=2.0).contains(&(font / size))
                    && angle.min(360.0 - angle) <= 2.0
                    && [
                        baseline.start.x,
                        baseline.start.y,
                        baseline.end.x,
                        baseline.end.y,
                    ]
                    .into_iter()
                    .all(f64::is_finite))
                .then_some((font, native))
            })
            .max_by(|(left, _), (right, _)| left.total_cmp(right))
            .map(|(_, native)| native);
        covered.sort_by_key(|matched| (matched.bytes.start, matched.bytes.end));
        let mut remaining = Vec::new();
        let (mut byte_start, mut inline_start) = (0, 0.0_f64);
        for matched in covered {
            let (range, (start, end)) = (matched.bytes, matched.inline);
            if range.start > byte_start {
                remaining
                    .push((byte_start..range.start, (inline_start, start)));
            }
            byte_start = byte_start.max(range.end);
            inline_start = inline_start.max(end);
        }
        if byte_start < self.raw_text.len() {
            remaining
                .push((byte_start..self.raw_text.len(), (inline_start, 1.0)));
        }
        remaining
            .into_iter()
            .filter(|(range, _)| {
                self.raw_text
                    .get(range.clone())
                    .is_some_and(|s| !s.trim().is_empty())
            })
            .enumerate()
            .filter_map(|(index, (range, interval))| {
                self.ocr_fragment(range, interval, rtl, index, anchor)
            })
            .collect()
    }

    /// Aligns normalized native glyph runs with original OCR byte ranges in one shared reading frame.
    fn native_matches<'a>(
        &self,
        mut native: Vec<&'a Self>,
        axes: TextAxes,
        frame: Bbox,
        rtl: bool,
    ) -> Vec<NativeMatch<'a>> {
        native.sort_by(|left, right| {
            let order = axes
                .project(left.bbox.center())
                .x
                .total_cmp(&axes.project(right.bbox.center()).x);
            (if rtl { order.reverse() } else { order })
                .then_with(|| left.id.cmp(&right.id))
        });
        // Retain original UTF-8 boundaries while comparing the same punctuation/case/space forms.
        let normalize = |text: &str| -> Vec<(char, Range<usize>)> {
            text.char_indices()
                .filter(|(_, c)| !c.is_whitespace())
                .flat_map(|(index, c)| {
                    crate::text_rules::normalize_punctuation(c)
                        .to_lowercase()
                        .map(move |normalized| {
                            (normalized, index..index + c.len_utf8())
                        })
                })
                .collect()
        };
        let text = normalize(&self.raw_text);
        let mut covered: Vec<NativeMatch<'a>> = Vec::new();
        for native in native {
            let pattern = normalize(&native.raw_text);
            if pattern.is_empty() || pattern.len() > text.len() {
                continue;
            }
            let Ok(bounds) = axes.project_bbox(native.bbox) else {
                continue;
            };
            let (start, end) = if rtl {
                (
                    (frame.right - bounds.right) / frame.width(),
                    (frame.right - bounds.left) / frame.width(),
                )
            } else {
                (
                    (bounds.left - frame.left) / frame.width(),
                    (bounds.right - frame.left) / frame.width(),
                )
            };
            let center = (start + end) * 0.5;
            // Adjacent PDF glyph runs establish a stronger anchor than uniform character-width estimates.
            // This also prevents a repeated native glyph from reusing the preceding glyph's text match.
            let position_error =
                |index: usize, candidate: &[(char, Range<usize>)]| {
                    let adjacent = covered.last().is_some_and(|previous| {
                        ((start - previous.inline.1) * frame.width()).abs()
                            <= frame.height()
                            && candidate.first().is_some_and(|(_, range)| {
                                self.raw_text
                                    .get(previous.bytes.end..range.start)
                                    .is_some_and(|gap| {
                                        gap.chars().all(char::is_whitespace)
                                    })
                            })
                    });
                    if adjacent {
                        0.0
                    } else {
                        ((index as f64 + pattern.len() as f64 * 0.5)
                            / text.len() as f64
                            - center)
                            .abs()
                    }
                };
            let matching = text
                .windows(pattern.len())
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate
                        .iter()
                        .zip(&pattern)
                        .all(|(left, right)| left.0 == right.0)
                })
                .min_by(|(left, a), (right, b)| {
                    position_error(*left, a)
                        .total_cmp(&position_error(*right, b))
                })
                .filter(|(index, candidate)| {
                    position_error(*index, candidate)
                        <= 0.25 + frame.height() / frame.width()
                });
            let Some((_, matching)) = matching else {
                continue;
            };
            let (Some(first), Some(last)) = (matching.first(), matching.last())
            else {
                continue;
            };
            let mut range = first.1.start..last.1.end;
            // Native whitespace remains immutable; remove its OCR copy at the same boundary.
            if native.raw_text.starts_with(char::is_whitespace) {
                range.start = self
                    .raw_text
                    .get(..range.start)
                    .unwrap_or_default()
                    .trim_end()
                    .len();
            }
            if native.raw_text.ends_with(char::is_whitespace) {
                let tail = self.raw_text.get(range.end..).unwrap_or_default();
                range.end += tail.len() - tail.trim_start().len();
            }
            covered.push(NativeMatch {
                bytes: range,
                inline: (start.clamp(0.0, 1.0), end.clamp(0.0, 1.0)),
                item: native,
            });
        }
        covered
    }

    /// Slices the reading quad at native boundaries before canonical line/table byte ranges are assigned.
    fn ocr_fragment(
        &self,
        range: Range<usize>,
        (mut start, mut end): (f64, f64),
        rtl: bool,
        index: usize,
        anchor: Option<&Self>,
    ) -> Option<Self> {
        let text = self.raw_text.get(range.clone())?;
        if end <= start {
            // Overlapping PDF boxes can hide the gap; proportional geometry preserves text without inventing deletion.
            let count = self.raw_text.chars().count().max(1) as f64;
            start = self.raw_text.get(..range.start)?.chars().count() as f64
                / count;
            end = start + text.chars().count() as f64 / count;
        }
        if rtl {
            (start, end) = (1.0 - end, 1.0 - start);
        }
        let corners = [
            Point::new(self.bbox.left, self.bbox.top),
            Point::new(self.bbox.right, self.bbox.top),
            Point::new(self.bbox.right, self.bbox.bottom),
            Point::new(self.bbox.left, self.bbox.bottom),
        ];
        // Custom engines may supply a non-quadrilateral polygon; its validated envelope still permits slicing.
        let points: &[Point; 4] = self
            .polygon
            .as_ref()
            .and_then(|polygon| polygon.points().try_into().ok())
            .unwrap_or(&corners);
        let [a, b, c, d] = *points;
        let interpolate = |a: Point, b: Point, t: f64| {
            Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
        };
        let quad = [
            interpolate(a, b, start),
            interpolate(a, b, end),
            interpolate(d, c, end),
            interpolate(d, c, start),
        ];
        let mut fragment = self.clone();
        fragment.id = self.id.ocr_fragment(index);
        fragment.raw_text = text.to_owned();
        fragment.polygon = Polygon::try_from(quad.to_vec()).ok();
        fragment.bbox =
            fragment.polygon.as_ref().map_or(self.bbox, Polygon::bbox);
        fragment.baseline = Some(Baseline {
            start: quad[3],
            end: quad[2],
        });
        if let Some(anchor) = anchor
            && let Some(baseline) = anchor.baseline
        {
            // Calibrate only OCR hints; native facts and the OCR polygon remain unchanged.
            let axes = TextAxes::from(anchor.rotation);
            let y = axes.project(baseline.start).y;
            fragment.baseline = Some(Baseline {
                start: axes.unproject(Point::new(axes.project(quad[3]).x, y)),
                end: axes.unproject(Point::new(axes.project(quad[2]).x, y)),
            });
            fragment.rotation = anchor.rotation;
            if let (Some(style), Some(measured)) =
                (fragment.style.as_mut(), anchor.style.as_ref())
            {
                style.font_size = measured.font_size;
                style.font_height = measured
                    .font_height
                    .filter(|height| height.is_finite() && *height > 0.0)
                    .or(measured.font_size);
                style.font_size_estimated = true;
            }
        }
        fragment.repair_actions.push(RepairAction::OcrNativeOverlap);
        Some(fragment)
    }
}
