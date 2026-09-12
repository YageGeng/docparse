use docparse_layout::Bbox;

use crate::TextItem;

/// Borrowed page facts sorted by top edge, with prefix maxima retaining tall overlapping spans.
pub(super) struct TextIndex<'a> {
    items: Vec<&'a TextItem>,
    max_bottom: Vec<f64>,
}

impl<'a> FromIterator<&'a TextItem> for TextIndex<'a> {
    /// Builds one reusable page index without copying strings, styles or provenance.
    fn from_iter<T: IntoIterator<Item = &'a TextItem>>(items: T) -> Self {
        let mut items: Vec<_> = items.into_iter().collect();
        items.sort_by(|a, b| {
            a.bbox
                .top
                .total_cmp(&b.bbox.top)
                .then_with(|| a.id.cmp(&b.id))
        });
        let max_bottom = items
            .iter()
            .scan(f64::NEG_INFINITY, |bottom, item| {
                *bottom = bottom.max(item.bbox.bottom);
                Some(*bottom)
            })
            .collect();
        Self { items, max_bottom }
    }
}

impl<'a> TextIndex<'a> {
    /// Narrows the vertical interval before exact overlap checks; never drops an earlier tall span.
    pub(super) fn overlapping(
        &self,
        bbox: Bbox,
    ) -> impl Iterator<Item = &'a TextItem> + '_ {
        let begin = self
            .max_bottom
            .partition_point(|bottom| *bottom <= bbox.top);
        let end = self
            .items
            .partition_point(|item| item.bbox.top < bbox.bottom);
        // ponytail: prefix maxima are conservative; an interval tree is only needed if many page-height spans dominate.
        self.items
            .get(begin..end)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(move |item| item.bbox.intersection_area(bbox) > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index must equal a complete scan for nested, tall, disjoint and edge-touching boxes.
    #[test]
    fn interval_queries_preserve_all_overlaps() {
        let items: Vec<_> = (0..80)
            .rev()
            .map(|index| {
                TextItem::builder()
                    .id(crate::TextItemId::native(1, index))
                    .raw_text("text".into())
                    .bbox(
                        Bbox::try_from([
                            f64::from(index % 4) * 25.0,
                            f64::from(index) * 10.0,
                            f64::from(index % 4) * 25.0 + 20.0,
                            if index == 0 {
                                1000.0
                            } else {
                                f64::from(index) * 10.0 + 15.0
                            },
                        ])
                        .expect("box"),
                    )
                    .source(crate::TextSource::Native)
                    .build()
            })
            .collect();
        let index: TextIndex<'_> = items.iter().collect();
        for bounds in [
            [0.0, 600.0, 100.0, 630.0],
            [20.0, 30.0, 25.0, 50.0],
            [0.0, 995.0, 10.0, 1005.0],
            [100.0, 0.0, 110.0, 1000.0],
        ] {
            let bbox = Bbox::try_from(bounds).expect("query");
            let mut actual: Vec<_> = index
                .overlapping(bbox)
                .map(|item| item.id.as_str())
                .collect();
            let mut expected: Vec<_> = items
                .iter()
                .filter(|item| item.bbox.intersection_area(bbox) > 0.0)
                .map(|item| item.id.as_str())
                .collect();
            actual.sort_unstable();
            expected.sort_unstable();
            assert_eq!(actual, expected);
        }
    }
}
