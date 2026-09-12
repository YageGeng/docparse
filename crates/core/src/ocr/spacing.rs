use docparse_layout::Bbox;

use crate::line::{TextAxes, bidi::detect_direction};
use crate::{RepairAction, TextItem, WritingDirection};

/// Immutable word boundaries are computed once, before any source string is changed.
struct Word<'a> {
    item: &'a TextItem,
    starts_word: bool,
    ends_word: bool,
}

impl<'a> From<&'a TextItem> for Word<'a> {
    /// Caches Unicode and punctuation decisions while preserving explicit spaces and CJK boundaries.
    fn from(item: &'a TextItem) -> Self {
        let word = |c: char| {
            c.is_alphanumeric()
                && !matches!(c as u32,
            0x2e80..=0xa4cf | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x20000..=0x323af)
        };
        Self {
            item,
            starts_word: item.raw_text.chars().next().is_some_and(word),
            ends_word: item
                .raw_text
                .trim_end_matches([
                    ',', '.', ':', ';', '!', '?', ')', ']', '}', '"', '\'', '…',
                ])
                .chars()
                .last()
                .is_some_and(word),
        }
    }
}

impl Word<'_> {
    /// Excludes incompatible facts before projecting their geometry in the general-angle path.
    fn can_precede(&self, right: &Self) -> bool {
        self.ends_word
            && self.item.id != right.item.id
            && (self.item.rotation - right.item.rotation).abs() <= 2.0
    }
}

/// Adds OCR-only word gaps using cached boundaries and indexed baselines for cardinal reading frames.
pub(super) fn space_words(items: &mut [TextItem]) {
    let words: Vec<_> = items.iter().map(Word::from).collect();
    let mut prefix = vec![false; items.len()];
    // Each index is built only when used. All members are projected in the requesting frame,
    // never in separate per-item frames, so nearby detector angles retain the original semantics.
    let mut cardinal: [Option<Vec<(Bbox, usize)>>; 4] =
        std::array::from_fn(|_| None);
    for (prefix, right) in prefix.iter_mut().zip(&words) {
        if !right.starts_word {
            continue;
        }
        let axes = TextAxes::from(right.item.rotation);
        let Ok(right_box) = axes.project_bbox(right.item.bbox) else {
            continue;
        };
        let rtl = detect_direction(
            std::slice::from_ref(right.item),
            right.item.rotation,
        ) == WritingDirection::RightToLeft;
        let adjacent = |left_box: Bbox| {
            let height = left_box.height().min(right_box.height());
            let gap = if rtl {
                left_box.left - right_box.right
            } else {
                right_box.left - left_box.right
            };
            (left_box.bottom - right_box.bottom).abs() < height * 0.3
                && gap > height * 0.15
                && gap < height * 1.25
        };
        let angle = right.item.rotation.rem_euclid(360.0).to_bits();
        let slot = [0.0_f64, 90.0, 180.0, 270.0]
            .iter()
            .position(|cardinal| cardinal.to_bits() == angle);
        *prefix = if let Some(cache) =
            slot.and_then(|slot| cardinal.get_mut(slot))
        {
            let rows = cache.get_or_insert_with(|| {
                let mut rows: Vec<_> = words
                    .iter()
                    .enumerate()
                    .filter(|(_, word)| word.ends_word)
                    .filter_map(|(index, word)| {
                        axes.project_bbox(word.item.bbox)
                            .ok()
                            .map(|bbox| (bbox, index))
                    })
                    .collect();
                rows.sort_by(|(a, _), (b, _)| a.bottom.total_cmp(&b.bottom));
                rows
            });
            // The smaller of the two heights bounds the original tolerance, so this range is conservative.
            let tolerance = right_box.height() * 0.3;
            let begin = rows.partition_point(|(bbox, _)| {
                bbox.bottom < right_box.bottom - tolerance
            });
            let end = rows.partition_point(|(bbox, _)| {
                bbox.bottom <= right_box.bottom + tolerance
            });
            rows.get(begin..end).unwrap_or_default().iter().any(
                |(bbox, index)| {
                    words
                        .get(*index)
                        .is_some_and(|left| left.can_precede(right))
                        && adjacent(*bbox)
                },
            )
        } else {
            // ponytail: uncommon oblique frames keep an exact scan; index them only if profiles justify it.
            words
                .iter()
                .filter(|left| left.can_precede(right))
                .any(|left| {
                    axes.project_bbox(left.item.bbox).is_ok_and(adjacent)
                })
        };
    }
    // Apply all decisions together so newly added spaces cannot suppress a later word boundary.
    for (item, prefix) in items.iter_mut().zip(prefix) {
        if prefix {
            item.raw_text.insert(0, ' ');
            item.repair_actions.push(RepairAction::OcrSpacing);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TextItemId, TextSource};

    /// OCR word gaps become explicit before byte ranges are fixed; CJK remains unspaced.
    #[test]
    fn word_spacing_preserves_cjk_boundaries() {
        let mut items: Vec<_> = [
            ("Hello", 0.0, 20.0),
            ("world", 23.0, 43.0),
            ("中文", 46.0, 66.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (text, left, right))| {
            TextItem::builder()
                .id(TextItemId::ocr(1, index as u32))
                .raw_text(text.into())
                .bbox(Bbox::try_from([left, 10.0, right, 20.0]).expect("box"))
                .source(TextSource::Ocr)
                .build()
        })
        .collect();
        space_words(&mut items);
        assert_eq!(
            items
                .iter()
                .map(|item| item.raw_text.as_str())
                .collect::<Vec<_>>(),
            ["Hello", " world", "中文"]
        );
        assert_eq!(
            items.get(1).expect("second word").repair_actions,
            [RepairAction::OcrSpacing]
        );
        assert!(items.last().expect("CJK").repair_actions.is_empty());
    }

    /// Preserves the former complete scan as an independent oracle for the optimized candidate search.
    fn reference_spacing(items: &mut [TextItem]) {
        let mut prefix = vec![false; items.len()];
        for (prefix, right) in prefix.iter_mut().zip(items.iter()) {
            // CJK and punctuation keep their recognized boundaries; native text is never inferred.
            let word = |c: char| {
                c.is_alphanumeric()
                    && !matches!(c as u32,
                0x2e80..=0xa4cf | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x20000..=0x323af)
            };
            if !right.raw_text.chars().next().is_some_and(word) {
                continue;
            }
            let axes = crate::line::TextAxes::from(right.rotation);
            let Ok(right_box) = axes.project_bbox(right.bbox) else {
                continue;
            };
            let rtl = crate::line::bidi::detect_direction(
                std::slice::from_ref(right),
                right.rotation,
            ) == crate::WritingDirection::RightToLeft;
            *prefix = items.iter().any(|left| {
                if left.id == right.id
                    || (left.rotation - right.rotation).abs() > 2.0
                    // Closing punctuation still ends a word; explicit spaces and CJK stay untouched.
                    || !left.raw_text.trim_end_matches([',', '.', ':', ';', '!', '?', ')', ']', '}', '"', '\'', '…'])
                        .chars().last().is_some_and(word)
                {
                    return false;
                }
                let Ok(left_box) = axes.project_bbox(left.bbox) else {
                    return false;
                };
                let height = left_box.height().min(right_box.height());
                let gap = if rtl {
                    left_box.left - right_box.right
                } else {
                    right_box.left - left_box.right
                };
                (left_box.bottom - right_box.bottom).abs() < height * 0.3
                    && gap > height * 0.15
                    && gap < height * 1.25
            });
        }
        for (item, prefix) in items.iter_mut().zip(prefix) {
            if prefix {
                item.raw_text.insert(0, ' ');
                item.repair_actions.push(RepairAction::OcrSpacing);
            }
        }
    }
    /// Indexing must match the former scan for mixed scripts, heights, directions and detector-angle jitter.
    #[test]
    fn indexed_spacing_matches_reference_scan() {
        for offset in [0.0, 10000.0] {
            let mut items: Vec<_> = (0..240)
                .map(|index| {
                    let base: f64 =
                        *[0.0, 90.0, 180.0, 270.0, 0.7, 179.5, 13.0, 359.0]
                            .get((index / 20) % 8)
                            .expect("orientation");
                    let rotation =
                        if index % 5 == 0 { base + 0.4 } else { base };
                    let text = [
                        "word", "after,", "中文", "with ", " space", "שלום",
                        "123", "note)",
                    ]
                    .get(index % 8)
                    .copied()
                    .expect("sample text");
                    let vertical = (base - 90.0).abs() < 1e-9
                        || (base - 270.0).abs() < 1e-9;
                    let (left, top, width, height) = if vertical {
                        (
                            offset + (index / 20) as f64 * 15.0,
                            offset + (index % 20) as f64 * 17.0,
                            9.0 + (index % 3) as f64,
                            13.0,
                        )
                    } else {
                        (
                            offset + (index % 20) as f64 * 17.0,
                            offset + (index / 20) as f64 * 15.0,
                            13.0,
                            9.0 + (index % 3) as f64,
                        )
                    };
                    TextItem::builder()
                        .id(crate::TextItemId::ocr(1, index as u32))
                        .raw_text(text.into())
                        .bbox(
                            Bbox::try_from([
                                left,
                                top,
                                left + width,
                                top + height,
                            ])
                            .expect("box"),
                        )
                        .rotation(rotation)
                        .source(crate::TextSource::Ocr)
                        .build()
                })
                .collect();
            let mut reference = items.clone();
            reference_spacing(&mut reference);
            space_words(&mut items);
            assert_eq!(items, reference);
            assert!(items.iter().any(|item| {
                item.repair_actions.contains(&RepairAction::OcrSpacing)
            }));
        }
    }
}
