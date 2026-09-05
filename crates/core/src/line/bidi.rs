use crate::{TextItem, WritingDirection};

/// Snaps near-cardinal rotations with LiteParse's circular two-degree tolerance.
fn canonical_rotation(rotation: f64) -> i32 {
    let rotation = rotation.rem_euclid(360.0);
    [0.0_f64, 90.0, 180.0, 270.0]
        .into_iter()
        .min_by(|left, right| {
            let left_delta =
                (rotation - left).abs().min(360.0 - (rotation - left).abs());
            let right_delta = (rotation - right)
                .abs()
                .min(360.0 - (rotation - right).abs());
            left_delta.total_cmp(&right_delta)
        })
        .filter(|candidate| {
            let raw_delta = (rotation - candidate).abs();
            raw_delta.min(360.0 - raw_delta) <= 2.0
        })
        .map_or_else(|| rotation.round() as i32, |candidate| candidate as i32)
}

/// Detects writing direction from rotation and strong Unicode characters.
pub(crate) fn detect_direction(
    items: &[TextItem],
    rotation: f64,
) -> WritingDirection {
    let rotation = canonical_rotation(rotation);
    if matches!(rotation, 90 | 270) {
        return WritingDirection::Vertical;
    }
    let mut left_to_right = 0_usize;
    let mut right_to_left = 0_usize;
    for character in items.iter().flat_map(|item| item.raw_text.chars()) {
        if is_rtl_character(character) {
            right_to_left += 1;
        } else if character.is_alphabetic() {
            left_to_right += 1;
        }
    }
    if right_to_left > left_to_right {
        WritingDirection::RightToLeft
    } else {
        WritingDirection::LeftToRight
    }
}

/// Orders text item containers without mutating text within an item.
pub(crate) fn order_items(items: &mut [TextItem], direction: WritingDirection) {
    let rotation = items
        .first()
        .map_or(0, |item| canonical_rotation(item.rotation));
    match direction {
        WritingDirection::LeftToRight => items.sort_by(|left, right| {
            left.bbox
                .left
                .total_cmp(&right.bbox.left)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        }),
        WritingDirection::RightToLeft => items.sort_by(|left, right| {
            right
                .bbox
                .right
                .total_cmp(&left.bbox.right)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        }),
        WritingDirection::Vertical if rotation == 270 => {
            // LiteParse maps y to `max_y - y - height` before an ascending
            // horizontal sort. Sorting original bottoms descending is equivalent
            // and preserves canonical page geometry.
            items.sort_by(|left, right| {
                right
                    .bbox
                    .bottom
                    .total_cmp(&left.bbox.bottom)
                    .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                    .then_with(|| left.id.as_str().cmp(right.id.as_str()))
            });
        }
        WritingDirection::Vertical => {
            items.sort_by(|left, right| {
                left.bbox
                    .top
                    .total_cmp(&right.bbox.top)
                    .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                    .then_with(|| left.id.as_str().cmp(right.id.as_str()))
            });
        }
    }
}

/// Returns true for strong Hebrew, Arabic, and presentation-form code points.
fn is_rtl_character(character: char) -> bool {
    matches!(
        character as u32,
        0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF
    )
}

#[cfg(test)]
mod tests {
    use docparse_layout::Bbox;

    use super::{canonical_rotation, detect_direction, order_items};
    use crate::{TextItem, TextItemId, TextSource, WritingDirection};

    /// Builds one positioned text item for direction tests.
    fn item(index: u32, text: &str, left: f64) -> TextItem {
        TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(text.to_owned())
            .bbox(
                Bbox::try_from([left, 10.0, left + 20.0, 20.0])
                    .expect("test bbox must be valid"),
            )
            .source(TextSource::Native)
            .build()
    }

    /// Builds one cardinally rotated item at a caller-selected vertical position.
    fn rotated_item(
        index: u32,
        text: &str,
        top: f64,
        rotation: f64,
    ) -> TextItem {
        TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(text.to_owned())
            .bbox(
                Bbox::try_from([10.0, top, 30.0, top + 10.0])
                    .expect("test bbox must be valid"),
            )
            .rotation(rotation)
            .source(TextSource::Native)
            .build()
    }

    /// Verifies strong RTL characters determine direction while numbers remain neutral.
    #[test]
    fn rtl_order_changes_items_without_reversing_text() {
        let mut items = vec![item(0, "שלום", 100.0), item(1, "123", 70.0)];
        let direction = detect_direction(&items, 0.0);

        order_items(&mut items, direction);

        assert_eq!(direction, WritingDirection::RightToLeft);
        assert_eq!(
            items.first().map(|item| item.raw_text.as_str()),
            Some("שלום")
        );
        assert_eq!(
            items.last().map(|item| item.raw_text.as_str()),
            Some("123")
        );
    }

    /// Verifies upright Latin and rotated text select distinct directions.
    #[test]
    fn ltr_and_vertical_directions_are_detected() {
        let latin = vec![item(0, "abcdefgh", 0.0), item(1, "שלום", 30.0)];

        assert_eq!(
            detect_direction(&latin, 0.0),
            WritingDirection::LeftToRight
        );
        assert_eq!(detect_direction(&latin, 90.0), WritingDirection::Vertical);
    }

    /// Verifies 270-degree text follows LiteParse's bottom-to-top virtual axis.
    #[test]
    fn rotation_270_orders_items_bottom_to_top() {
        let mut items = vec![
            rotated_item(2, "2024", 10.0, 270.0),
            rotated_item(0, "arXiv:", 50.0, 270.0),
            rotated_item(1, "2403.01632v4 ", 30.0, 270.0),
        ];
        let direction = detect_direction(&items, 270.0);

        order_items(&mut items, direction);

        assert_eq!(direction, WritingDirection::Vertical);
        assert_eq!(
            items
                .iter()
                .map(|item| item.raw_text.as_str())
                .collect::<String>(),
            "arXiv:2403.01632v4 2024"
        );
    }

    /// Verifies 90-degree text retains LiteParse's top-to-bottom virtual axis.
    #[test]
    fn rotation_90_orders_items_top_to_bottom() {
        let mut items = vec![
            rotated_item(2, "bottom", 50.0, 90.0),
            rotated_item(0, "top", 10.0, 90.0),
            rotated_item(1, "middle", 30.0, 90.0),
        ];
        let direction = detect_direction(&items, 90.0);

        order_items(&mut items, direction);

        assert_eq!(direction, WritingDirection::Vertical);
        assert_eq!(
            items
                .iter()
                .map(|item| item.raw_text.as_str())
                .collect::<Vec<_>>(),
            vec!["top", "middle", "bottom"]
        );
    }

    /// Verifies LiteParse-compatible circular snapping covers both cardinal directions.
    #[test]
    fn canonical_rotation_snaps_near_cardinal_angles() {
        assert_eq!(canonical_rotation(88.5), 90);
        assert_eq!(canonical_rotation(271.0), 270);
        assert_eq!(canonical_rotation(359.0), 0);
        assert_eq!(canonical_rotation(-1.0), 0);
        assert_eq!(canonical_rotation(45.0), 45);
    }
}
