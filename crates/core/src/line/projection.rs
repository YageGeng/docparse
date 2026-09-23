//! Geometry-based whitespace projection, following LiteParse's median-character-width approach.
use super::TextAxes;
use crate::{Line, WritingDirection};

impl Line {
    /// Emits physical row boundaries and spacing independently of text/Markdown encoding, borrowing source text when possible.
    pub(crate) fn project_layout<'a, T: AsRef<str>>(
        lines: &'a [Self],
        render: impl Fn(&'a Self) -> T,
        mut emit: impl FnMut(bool, usize, &str),
    ) {
        let Some(first) = lines.first() else {
            return;
        };
        let axes = TextAxes::from(first.rotation);
        let rtl = first.direction == WritingDirection::RightToLeft;
        let mut widths: Vec<_> = lines
            .iter()
            .flat_map(|line| &line.text_items)
            .filter_map(|item| {
                let count = item.raw_text.chars().count();
                let width = axes.project_bbox(item.bbox).ok()?.width();
                (count > 0 && width > 0.0)
                    .then_some(width / count.max(1) as f64)
            })
            .collect();
        // Selection is linear and needs no ordering beyond the two middle values.
        let count = widths.len();
        let char_width = if count == 0 {
            first.bbox.height() * 0.5
        } else {
            let (lower, middle, _) =
                widths.select_nth_unstable_by(count / 2, f64::total_cmp);
            if count.is_multiple_of(2) {
                (lower
                    .iter()
                    .copied()
                    .max_by(f64::total_cmp)
                    .unwrap_or(*middle)
                    + *middle)
                    * 0.5
            } else {
                *middle
            }
        }
        .max(0.1);
        let origin = lines
            .iter()
            .filter(|line| {
                !line.text.trim().is_empty() || !line.inline_spans.is_empty()
            })
            .filter_map(|line| axes.project_bbox(line.bbox).ok())
            .map(|bbox| if rtl { -bbox.right } else { bbox.left })
            .reduce(f64::min)
            .unwrap_or(0.0);
        let mut previous: Option<(&Line, docparse_layout::Bbox, f64)> = None;
        let mut columns = 0usize;
        for line in lines {
            // Render one line at a time rather than retaining another document-sized string collection.
            let rendered = render(line);
            let rendered = rendered.as_ref();
            if rendered.trim().is_empty() {
                continue;
            }
            let Ok(bbox) = axes.project_bbox(line.bbox) else {
                continue;
            };
            let baseline = line
                .baseline
                .map_or(bbox.bottom, |baseline| axes.project(baseline.start).y);
            let same_row = previous.is_some_and(|(prior, bounds, row)| {
                prior.direction == line.direction
                    && (prior.rotation - line.rotation).abs() < 2.0
                    && (baseline - row).abs()
                        <= bbox.height().min(bounds.height()) * 0.35
                    && if rtl {
                        bbox.right <= bounds.left + char_width
                    } else {
                        bbox.left >= bounds.right - char_width
                    }
            });
            if !same_row {
                columns = 0;
            }
            let start = if rtl { -bbox.right } else { bbox.left };
            // Bound pathological PDF coordinates while retaining LiteParse's Unicode character-column accounting.
            #[expect(
                clippy::cast_sign_loss,
                reason = "The finite column offset is clamped to a non-negative bounded range before conversion."
            )]
            let target = ((start - origin) / char_width)
                .round()
                .clamp(0.0, 4096.0) as usize;
            let leading = rendered.chars().take_while(|ch| *ch == ' ').count();
            let mut spaces = target.saturating_sub(columns).max(leading);
            if same_row && let Some((_, bounds, _)) = previous {
                let gap = if rtl {
                    bounds.left - bbox.right
                } else {
                    bbox.left - bounds.right
                };
                if gap > char_width * 0.5 {
                    spaces = spaces.max(1);
                }
            }
            emit(
                !same_row && previous.is_some(),
                spaces,
                rendered.trim_end().trim_start_matches(' '),
            );
            columns += spaces + line.text.trim().chars().count();
            previous = Some((line, bbox, baseline));
        }
    }
}
