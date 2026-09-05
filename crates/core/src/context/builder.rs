use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use typed_builder::TypedBuilder;

use crate::{ContextError, DocumentContext, PageProbe};

/// Collects page probes and freezes deterministic document-wide context.
#[derive(Debug, TypedBuilder)]
pub struct DocumentContextBuilder {
    page_count: u32,
    #[builder(default)]
    probes: Vec<PageProbe>,
    #[builder(default)]
    metadata: BTreeMap<String, String>,
    #[builder(default)]
    model_revision: Option<String>,
}

impl DocumentContextBuilder {
    /// Creates an empty collector for an exact document page count.
    pub fn new(page_count: u32) -> Self {
        Self::builder().page_count(page_count).build()
    }

    /// Adds one validated page probe without accepting duplicate page numbers.
    pub fn push_page(&mut self, probe: PageProbe) -> Result<(), ContextError> {
        if probe.page_number == 0
            || probe.page_number > self.page_count
            || !probe.width.is_finite()
            || probe.width <= 0.0
            || !probe.height.is_finite()
            || probe.height <= 0.0
        {
            return Err(ContextError::InvalidProbe {
                page_number: probe.page_number,
                reason: "page number and dimensions must be valid".to_owned(),
            });
        }
        if self
            .probes
            .iter()
            .any(|existing| existing.page_number == probe.page_number)
        {
            return Err(ContextError::DuplicatePage {
                page_number: probe.page_number,
            });
        }
        self.probes.push(probe);
        Ok(())
    }

    /// Sorts all probes and freezes aggregate font, chrome, and numbering facts.
    pub fn build(mut self) -> Result<Arc<DocumentContext>, ContextError> {
        if self.probes.len() != self.page_count as usize {
            return Err(ContextError::ProbeCount {
                expected: self.page_count,
                actual: self.probes.len(),
            });
        }
        self.probes.sort_by_key(|probe| probe.page_number);

        let mut font_histogram = BTreeMap::<i32, u64>::new();
        let mut header_counts = BTreeMap::<String, u32>::new();
        let mut footer_counts = BTreeMap::<String, u32>::new();
        let mut heading_sizes = Vec::new();
        for probe in &self.probes {
            for (bucket, count) in &probe.font_size_histogram {
                *font_histogram.entry(*bucket).or_insert(0) +=
                    u64::from(*count);
            }
            for fingerprint in
                BTreeSet::from_iter(probe.top_fingerprints.iter().cloned())
            {
                *header_counts.entry(fingerprint).or_insert(0) += 1;
            }
            for fingerprint in
                BTreeSet::from_iter(probe.bottom_fingerprints.iter().cloned())
            {
                *footer_counts.entry(fingerprint).or_insert(0) += 1;
            }
            heading_sizes.extend(probe.title_font_sizes.iter().copied());
        }
        let body_font_size = font_histogram
            .iter()
            .fold(None, |best, (bucket, count)| match best {
                Some((_, best_count)) if best_count >= *count => best,
                _ => Some((*bucket, *count)),
            })
            .map(|(bucket, _)| f64::from(bucket) / 10.0);
        let repeated_header_fingerprints =
            Self::repeated_fingerprints(header_counts, self.page_count);
        let repeated_footer_fingerprints =
            Self::repeated_fingerprints(footer_counts, self.page_count);
        let page_number_pattern = self
            .probes
            .iter()
            .enumerate()
            .all(|(index, probe)| {
                let Ok(page_number) = u32::try_from(index + 1) else {
                    return false;
                };
                probe
                    .page_number_candidates
                    .iter()
                    .any(|candidate| candidate == &page_number.to_string())
            })
            .then(|| "sequential-arabic".to_owned());
        if let Some(body_font_size) = body_font_size {
            heading_sizes.retain(|size| *size > body_font_size);
        }
        heading_sizes.sort_by(f64::total_cmp);
        heading_sizes
            .dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);

        Ok(Arc::new(
            DocumentContext::builder()
                .page_count(self.page_count)
                .body_font_size(body_font_size)
                .repeated_header_fingerprints(repeated_header_fingerprints)
                .repeated_footer_fingerprints(repeated_footer_fingerprints)
                .page_number_pattern(page_number_pattern)
                .heading_font_sizes(heading_sizes)
                .model_revision(self.model_revision)
                .metadata(self.metadata)
                .build(),
        ))
    }

    /// Selects fingerprints present on at least sixty percent and two pages.
    fn repeated_fingerprints(
        counts: BTreeMap<String, u32>,
        page_count: u32,
    ) -> Vec<String> {
        counts
            .into_iter()
            .filter(|(_, count)| *count >= 2 && *count * 5 >= page_count * 3)
            .map(|(fingerprint, _)| fingerprint)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{DocumentContextBuilder, PageProbe};

    /// Builds one page probe with stable font and chrome signals.
    fn probe(page_number: u32) -> PageProbe {
        PageProbe::builder()
            .page_number(page_number)
            .width(612.0)
            .height(792.0)
            .rotation(0)
            .font_size_histogram(BTreeMap::from([(90, 100), (160, 2)]))
            .top_fingerprints(vec!["running header".to_owned()])
            .bottom_fingerprints(vec![page_number.to_string()])
            .page_number_candidates(vec![page_number.to_string()])
            .title_font_sizes(vec![16.0])
            .build()
    }

    /// Verifies shuffled probes produce frozen document-level statistics.
    #[test]
    fn shuffled_probes_build_expected_context() {
        let mut builder = DocumentContextBuilder::new(3);
        for page_number in [3, 1, 2] {
            builder
                .push_page(probe(page_number))
                .expect("probe must be valid");
        }

        let context = builder.build().expect("context must build");

        assert_eq!(context.page_count, 3);
        assert_eq!(context.body_font_size, Some(9.0));
        assert_eq!(
            context.repeated_header_fingerprints,
            vec!["running header"]
        );
        assert!(context.repeated_footer_fingerprints.is_empty());
        assert_eq!(
            context.page_number_pattern.as_deref(),
            Some("sequential-arabic")
        );
        assert_eq!(context.heading_font_sizes, vec![16.0]);
    }

    /// Verifies probe insertion order cannot change canonical context JSON.
    #[test]
    fn context_serialization_is_insertion_order_independent() {
        let mut forward = DocumentContextBuilder::new(3);
        let mut reverse = DocumentContextBuilder::new(3);
        for page_number in [1, 2, 3] {
            forward
                .push_page(probe(page_number))
                .expect("probe must be valid");
        }
        for page_number in [3, 2, 1] {
            reverse
                .push_page(probe(page_number))
                .expect("probe must be valid");
        }

        let forward =
            serde_json::to_vec(&*forward.build().expect("context must build"))
                .expect("context must serialize");
        let reverse =
            serde_json::to_vec(&*reverse.build().expect("context must build"))
                .expect("context must serialize");

        assert_eq!(forward, reverse);
    }
}
