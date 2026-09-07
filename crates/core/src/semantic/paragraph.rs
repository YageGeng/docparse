use docparse_config::FusionConfig;

use crate::RepairAction;
use crate::line::{LineAnchor, LineFragment};

/// One explainable decision between adjacent local lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParagraphDecision {
    Continue,
    Split { reason: &'static str },
    EvidenceOnly { hint: &'static str },
}

/// Stable first-version paragraph policy applied only inside one source region.
#[derive(Debug, Clone)]
pub(crate) struct ParagraphSplitter {
    config: FusionConfig,
}

impl ParagraphSplitter {
    /// Creates a splitter from validated fusion thresholds.
    pub(crate) const fn new(config: FusionConfig) -> Self {
        Self { config }
    }

    /// Classifies the boundary between two already ordered local line fragments.
    pub(crate) fn between(
        &self,
        previous: &LineFragment,
        next: &LineFragment,
    ) -> ParagraphDecision {
        let angle_difference =
            (previous.rotation - next.rotation).abs().rem_euclid(360.0);
        if angle_difference.min(360.0 - angle_difference) > 2.0 {
            return ParagraphDecision::Split {
                reason: "orientation_transition",
            };
        }
        let previous_centered = previous.metrics.anchor == LineAnchor::Center;
        let next_centered = next.metrics.anchor == LineAnchor::Center;
        if previous_centered != next_centered {
            return ParagraphDecision::Split {
                reason: "anchor_transition",
            };
        }

        let estimated = previous.metrics.font_size_estimated
            || next.metrics.font_size_estimated;
        let font_tolerance = if estimated {
            self.config.estimated_font_size_tolerance_points
        } else {
            self.config.font_size_tolerance_points
        };
        if (previous.metrics.font_size - next.metrics.font_size).abs()
            > font_tolerance
        {
            return ParagraphDecision::Split {
                reason: "font_size_transition",
            };
        }

        let strong_bold_transition = (previous.metrics.bold_ratio >= 0.75
            && next.metrics.bold_ratio <= 0.25)
            || (previous.metrics.bold_ratio <= 0.25
                && next.metrics.bold_ratio >= 0.75);
        if strong_bold_transition {
            return ParagraphDecision::Split {
                reason: "bold_transition",
            };
        }

        // A recovery from an indented first line is allowed; only a sudden right indent splits.
        if next.metrics.indent - previous.metrics.indent
            > self.config.indent_tolerance_points
        {
            return ParagraphDecision::Split {
                reason: "right_indent",
            };
        }

        let gap = next.bbox.top - previous.bbox.bottom;
        let maximum_height = previous.bbox.height().max(next.bbox.height());
        if gap > self.config.paragraph_gap_multiplier * maximum_height {
            return ParagraphDecision::Split {
                reason: "vertical_gap",
            };
        }

        let ends_encoded_hyphen = previous.ends_encoded_hyphen();
        if ends_encoded_hyphen {
            return ParagraphDecision::EvidenceOnly {
                hint: "encoded_hyphen_continuation",
            };
        }

        if next.starts_list_item() && !ends_encoded_hyphen {
            return ParagraphDecision::Split {
                reason: "list_boundary",
            };
        }

        ParagraphDecision::Continue
    }
}

impl LineFragment {
    /// Returns whether the final source fact is an isolated encoded hyphen.
    fn ends_encoded_hyphen(&self) -> bool {
        self.items.last().is_some_and(|item| {
            item.raw_text == "-"
                && item.repair_actions.contains(&RepairAction::EncodedHyphen)
        })
    }

    /// Returns whether source text begins with a conservative list marker.
    fn starts_list_item(&self) -> bool {
        let Some(text) = self
            .items
            .iter()
            .map(|item| item.raw_text.as_str())
            .find(|text| !text.trim().is_empty())
        else {
            return false;
        };
        let trimmed = text.trim_start();
        if ["- ", "* ", "• ", "– "]
            .iter()
            .any(|marker| trimmed.starts_with(marker))
        {
            return true;
        }
        let digit_count =
            trimmed.chars().take_while(char::is_ascii_digit).count();
        digit_count > 0
            && trimmed.get(digit_count..).is_some_and(|suffix| {
                suffix.starts_with(". ") || suffix.starts_with(") ")
            })
    }
}

#[cfg(test)]
mod tests {
    use docparse_config::FusionConfig;
    use docparse_layout::{Bbox, Point};

    use super::{ParagraphDecision, ParagraphSplitter};
    use crate::line::metrics::LineMetrics;
    use crate::line::{LineAnchor, LineFragment};
    use crate::{
        Baseline, RepairAction, TextItem, TextItemId, TextSource, TextStyle,
        WritingDirection,
    };

    /// Builds one line with caller-selected paragraph metrics.
    fn line(
        index: u32,
        text: &str,
        bounds: [f64; 4],
        font_size: f64,
        anchor: LineAnchor,
        bold_ratio: f64,
        estimated: bool,
    ) -> LineFragment {
        let bbox = Bbox::try_from(bounds).expect("test bbox must be valid");
        let baseline = Baseline {
            start: Point::new(bbox.left, bbox.bottom),
            end: Point::new(bbox.right, bbox.bottom),
        };
        let item = TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(text.to_owned())
            .bbox(bbox)
            .source(TextSource::Native)
            .style(Some(
                TextStyle::builder()
                    .font_size(Some(font_size))
                    .font_size_estimated(estimated)
                    .bold(bold_ratio >= 0.5)
                    .build(),
            ))
            .build();
        let metrics = LineMetrics::builder()
            .font_size(font_size)
            .font_size_estimated(estimated)
            .bold_ratio(bold_ratio)
            .italic_ratio(0.0)
            .bbox(bbox)
            .baseline(baseline)
            .anchor(anchor)
            .indent(bbox.left)
            .build();
        LineFragment::builder()
            .items(vec![item])
            .bbox(bbox)
            .baseline(baseline)
            .direction(WritingDirection::LeftToRight)
            .metrics(metrics)
            .rotation(0.0)
            .build()
    }

    /// Verifies a normal multilingual line transition remains in one paragraph.
    #[test]
    fn normal_chinese_transition_continues() {
        let first = line(
            0,
            "这是第一行。",
            [10.0, 10.0, 90.0, 20.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );
        let second = line(
            1,
            "这是第二行",
            [10.0, 22.0, 90.0, 32.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );

        assert_eq!(
            ParagraphSplitter::new(FusionConfig::default())
                .between(&first, &second),
            ParagraphDecision::Continue
        );
    }

    /// Verifies a large vertical gap creates a deterministic paragraph split.
    #[test]
    fn large_gap_splits() {
        let first = line(
            0,
            "first",
            [10.0, 10.0, 90.0, 20.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );
        let second = line(
            1,
            "second",
            [10.0, 40.0, 90.0, 50.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );

        assert!(matches!(
            ParagraphSplitter::new(FusionConfig::default())
                .between(&first, &second),
            ParagraphDecision::Split { .. }
        ));
    }

    /// Keeps independently oriented residual text out of an otherwise matching paragraph.
    #[test]
    fn orientation_changes_split_residual_paragraphs() {
        let first = line(
            0,
            "body",
            [10.0, 10.0, 90.0, 20.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );
        let mut second = line(
            1,
            "overlay",
            [10.0, 22.0, 90.0, 32.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );
        let splitter = ParagraphSplitter::new(FusionConfig::default());
        second.rotation = 315.0;
        assert_eq!(
            splitter.between(&first, &second),
            ParagraphDecision::Split {
                reason: "orientation_transition"
            }
        );
        second.rotation = 359.0;
        assert_eq!(
            splitter.between(&first, &second),
            ParagraphDecision::Continue
        );
    }

    /// Verifies center-anchor and strong style transitions split flow text.
    #[test]
    fn anchor_and_bold_transitions_split() {
        let left = line(
            0,
            "body",
            [10.0, 10.0, 90.0, 20.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );
        let centered = line(
            1,
            "heading",
            [20.0, 22.0, 80.0, 32.0],
            10.0,
            LineAnchor::Center,
            1.0,
            false,
        );

        assert!(matches!(
            ParagraphSplitter::new(FusionConfig::default())
                .between(&left, &centered),
            ParagraphDecision::Split { .. }
        ));
    }

    /// Verifies first-line indent recovery and encoded hyphen continuation stay together.
    #[test]
    fn indent_recovery_and_encoded_hyphen_continue() {
        let mut first = line(
            0,
            "hyphen",
            [20.0, 10.0, 90.0, 20.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );
        first.items.push(
            TextItem::builder()
                .id(TextItemId::native(1, 1))
                .raw_text("-".to_owned())
                .bbox(
                    Bbox::try_from([90.0, 10.0, 93.0, 20.0])
                        .expect("test hyphen bbox must be valid"),
                )
                .source(TextSource::Native)
                .repair_actions(vec![RepairAction::EncodedHyphen])
                .build(),
        );
        let second = line(
            1,
            "ated",
            [10.0, 22.0, 90.0, 32.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );

        assert!(matches!(
            ParagraphSplitter::new(FusionConfig::default())
                .between(&first, &second),
            ParagraphDecision::EvidenceOnly {
                hint: "encoded_hyphen_continuation"
            }
        ));
    }

    /// Verifies true and estimated font-size tolerances use separate thresholds.
    #[test]
    fn font_tolerance_respects_estimated_status() {
        let first = line(
            0,
            "first",
            [10.0, 10.0, 90.0, 20.0],
            10.0,
            LineAnchor::Left,
            0.0,
            false,
        );
        let true_change = line(
            1,
            "next",
            [10.0, 22.0, 90.0, 32.0],
            10.6,
            LineAnchor::Left,
            0.0,
            false,
        );
        let estimated_change = line(
            2,
            "next",
            [10.0, 22.0, 90.0, 32.0],
            11.4,
            LineAnchor::Left,
            0.0,
            true,
        );
        let splitter = ParagraphSplitter::new(FusionConfig::default());

        assert!(matches!(
            splitter.between(&first, &true_change),
            ParagraphDecision::Split { .. }
        ));
        assert_eq!(
            splitter.between(&first, &estimated_change),
            ParagraphDecision::Continue
        );
    }
}
