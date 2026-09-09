use std::collections::BTreeSet;

use docparse_layout::Bbox;

use crate::{
    DocumentResult, NodeRef, PageResult, TextItemRange, ValidationError,
};

const GEOMETRY_TOLERANCE: f64 = 0.01;

/// Validates canonical schema, identity, nesting, and finite geometry invariants.
pub struct ResultValidator;

impl ResultValidator {
    /// Validates a complete document and reports the first stable node path failure.
    pub fn validate(document: &DocumentResult) -> Result<(), ValidationError> {
        if document.context.page_count as usize != document.pages.len() {
            return Err(Self::invalid(
                "context.page_count",
                format!(
                    "expected {}, got {}",
                    document.pages.len(),
                    document.context.page_count
                ),
            ));
        }
        if let Some(body_font_size) = document.context.body_font_size
            && (!body_font_size.is_finite() || body_font_size <= 0.0)
        {
            return Err(Self::invalid(
                "context.body_font_size",
                "must be finite and greater than zero",
            ));
        }

        let mut block_ids = BTreeSet::new();
        let mut line_ids = BTreeSet::new();
        let mut text_item_ids = BTreeSet::new();
        for (page_index, page) in document.pages.iter().enumerate() {
            let expected_page_number =
                u32::try_from(page_index + 1).map_err(|_source| {
                    Self::invalid(
                        format!("pages[{page_index}].page_number"),
                        "page index exceeds u32",
                    )
                })?;
            let page_path = format!("pages[{page_index}]");
            if page.page_number != expected_page_number {
                return Err(Self::invalid(
                    format!("{page_path}.page_number"),
                    format!(
                        "expected {expected_page_number}, got {}",
                        page.page_number
                    ),
                ));
            }
            Self::validate_page_node(
                page,
                &page_path,
                &mut block_ids,
                &mut line_ids,
                &mut text_item_ids,
            )?;
        }
        Self::validate_relations(document, &block_ids, &line_ids)?;
        Ok(())
    }

    /// Validates one standalone page without imposing document array position.
    pub fn validate_page(page: &PageResult) -> Result<(), ValidationError> {
        let mut block_ids = BTreeSet::new();
        let mut line_ids = BTreeSet::new();
        let mut text_item_ids = BTreeSet::new();
        Self::validate_page_node(
            page,
            "page",
            &mut block_ids,
            &mut line_ids,
            &mut text_item_ids,
        )
    }

    /// Validates one page and recursively records globally unique child IDs.
    fn validate_page_node(
        page: &PageResult,
        path: &str,
        block_ids: &mut BTreeSet<String>,
        line_ids: &mut BTreeSet<String>,
        text_item_ids: &mut BTreeSet<String>,
    ) -> Result<(), ValidationError> {
        if !page.width.is_finite() || page.width <= 0.0 {
            return Err(Self::invalid(
                format!("{path}.width"),
                "must be finite and greater than zero",
            ));
        }
        if !page.height.is_finite() || page.height <= 0.0 {
            return Err(Self::invalid(
                format!("{path}.height"),
                "must be finite and greater than zero",
            ));
        }
        if !matches!(page.rotation, 0 | 90 | 180 | 270) {
            return Err(Self::invalid(
                format!("{path}.rotation"),
                "must be 0, 90, 180, or 270 degrees",
            ));
        }

        // A merged block keeps a primary identity while every original model region
        // still contributes to exactly one final owner.
        let mut model_region_ids = BTreeSet::new();
        let mut source_model_region_ids = BTreeSet::new();
        for (block_index, block) in page.blocks.iter().enumerate() {
            let block_path = format!("{path}.blocks[{block_index}]");
            if block.final_order as usize != block_index {
                return Err(Self::invalid(
                    &block_path,
                    format!(
                        "final_order must equal array index {block_index}, got {}",
                        block.final_order
                    ),
                ));
            }
            Self::validate_id_page(
                block.id.as_str(),
                page.page_number,
                &block_path,
            )?;
            if !block_ids.insert(block.id.as_str().to_owned()) {
                return Err(Self::invalid(&block_path, "duplicate BlockId"));
            }
            Self::validate_bbox(block.bbox, &format!("{block_path}.bbox"))?;
            if block.label == docparse_layout::LayoutLabel::Reference
                && (!block.text.is_empty()
                    || !block.lines.is_empty()
                    || !block.source_regions.is_empty())
            {
                return Err(Self::invalid(
                    &block_path,
                    "reference annotations must not own text or merged layouts",
                ));
            }
            if let Some(polygon) = &block.polygon
                && !Self::bbox_contains(block.bbox, polygon.bbox())
            {
                return Err(Self::invalid(
                    format!("{block_path}.polygon"),
                    "polygon must stay inside the conservative Block bbox",
                ));
            }
            if block.label == docparse_layout::LayoutLabel::Watermark
                && block.model_region_id.is_some()
            {
                return Err(Self::invalid(
                    &block_path,
                    "watermark must not own a model region",
                ));
            }
            Self::validate_optional_confidence(
                block.confidence,
                &format!("{block_path}.confidence"),
            )?;
            if let Some(model_region_id) = &block.model_region_id {
                Self::validate_id_page(
                    model_region_id.as_str(),
                    page.page_number,
                    &format!("{block_path}.model_region_id"),
                )?;
                if !model_region_ids.insert(model_region_id.as_str().to_owned())
                {
                    return Err(Self::invalid(
                        format!("{block_path}.model_region_id"),
                        "must identify exactly one final model Block",
                    ));
                }
            }
            if !block.source_regions.is_empty()
                && block.source_region.as_ref().is_none_or(|primary| {
                    !block.source_regions.contains(primary)
                })
            {
                return Err(Self::invalid(
                    format!("{block_path}.source_regions"),
                    "merged regions must retain the primary source",
                ));
            }
            for (source_index, source_region) in
                block.source_regions().enumerate()
            {
                let source_path = if block.source_regions.is_empty() {
                    format!("{block_path}.source_region")
                } else {
                    format!("{block_path}.source_regions[{source_index}]")
                };
                Self::validate_bbox(
                    source_region.bbox,
                    &format!("{source_path}.bbox"),
                )?;
                let source_count =
                    usize::from(source_region.model_region_id.is_some())
                        + usize::from(
                            source_region.fallback_region_id.is_some(),
                        );
                if source_count != 1 {
                    return Err(Self::invalid(
                        &source_path,
                        "must reference exactly one model or fallback region",
                    ));
                }
                if let Some(id) = &source_region.model_region_id {
                    Self::validate_id_page(
                        id.as_str(),
                        page.page_number,
                        &source_path,
                    )?;
                    if !source_model_region_ids.insert(id.as_str()) {
                        return Err(Self::invalid(
                            &source_path,
                            "model source must contribute to exactly one final Block",
                        ));
                    }
                }
                if let Some(id) = &source_region.fallback_region_id {
                    Self::validate_id_page(
                        id.as_str(),
                        page.page_number,
                        &source_path,
                    )?;
                }
            }

            for (line_index, line) in block.lines.iter().enumerate() {
                let line_path = format!("{block_path}.lines[{line_index}]");
                Self::validate_id_page(
                    line.id.as_str(),
                    page.page_number,
                    &line_path,
                )?;
                if !line.id.as_str().starts_with(block.id.as_str()) {
                    return Err(Self::invalid(
                        &line_path,
                        "LineId parent does not match BlockId",
                    ));
                }
                if !line_ids.insert(line.id.as_str().to_owned()) {
                    return Err(Self::invalid(&line_path, "duplicate LineId"));
                }
                Self::validate_bbox(line.bbox, &format!("{line_path}.bbox"))?;
                if !Self::bbox_contains(block.bbox, line.bbox) {
                    return Err(Self::invalid(
                        format!("{line_path}.bbox"),
                        "line must be inside final Block bbox tolerance",
                    ));
                }
                Self::validate_optional_confidence(
                    line.model_region_coverage,
                    &format!("{line_path}.model_region_coverage"),
                )?;

                let mut raw_text_offset = 0_usize;
                for (item_index, item) in line.text_items.iter().enumerate() {
                    let item_path =
                        format!("{line_path}.text_items[{item_index}]");
                    if item.final_order as usize != item_index {
                        return Err(Self::invalid(
                            &item_path,
                            format!(
                                "final_order must equal array index {item_index}, got {}",
                                item.final_order
                            ),
                        ));
                    }
                    Self::validate_id_page(
                        item.id.as_str(),
                        page.page_number,
                        &item_path,
                    )?;
                    if !text_item_ids.insert(item.id.as_str().to_owned()) {
                        return Err(Self::invalid(
                            &item_path,
                            "duplicate TextItemId ownership",
                        ));
                    }
                    Self::validate_bbox(
                        item.bbox,
                        &format!("{item_path}.bbox"),
                    )?;
                    if item.watermark.is_some()
                        != (block.label
                            == docparse_layout::LayoutLabel::Watermark)
                    {
                        return Err(Self::invalid(
                            &item_path,
                            "watermark facts must belong exclusively to a watermark block",
                        ));
                    }
                    if let Some(polygon) = &item.polygon
                        && !Self::bbox_contains(item.bbox, polygon.bbox())
                    {
                        return Err(Self::invalid(
                            &item_path,
                            "text polygon must stay inside its bbox",
                        ));
                    }
                    if !Self::bbox_contains(line.bbox, item.bbox) {
                        return Err(Self::invalid(
                            format!("{item_path}.bbox"),
                            "text item must be inside final Line bbox tolerance",
                        ));
                    }
                    Self::validate_optional_confidence(
                        item.confidence,
                        &format!("{item_path}.confidence"),
                    )?;
                    if !item.rotation.is_finite() {
                        return Err(Self::invalid(
                            format!("{item_path}.rotation"),
                            "must be finite",
                        ));
                    }
                    // Validate the raw projection incrementally so production validation
                    // does not allocate a duplicate string for every completed line.
                    let Some(raw_text_end) =
                        raw_text_offset.checked_add(item.raw_text.len())
                    else {
                        return Err(Self::invalid(
                            format!("{line_path}.text"),
                            "raw text length overflow",
                        ));
                    };
                    if line.text.get(raw_text_offset..raw_text_end)
                        != Some(item.raw_text.as_str())
                    {
                        return Err(Self::invalid(
                            format!("{line_path}.text"),
                            "must concatenate ordered TextItem raw_text exactly",
                        ));
                    }
                    raw_text_offset = raw_text_end;
                }
                if raw_text_offset != line.text.len() {
                    return Err(Self::invalid(
                        format!("{line_path}.text"),
                        "must concatenate ordered TextItem raw_text exactly",
                    ));
                }
                for (span_index, span) in line.inline_spans.iter().enumerate() {
                    let span_path =
                        format!("{line_path}.inline_spans[{span_index}]");
                    Self::validate_bbox(
                        span.bbox,
                        &format!("{span_path}.bbox"),
                    )?;
                    Self::validate_text_range(
                        span.text_item_range,
                        line.text_items.len(),
                        &format!("{span_path}.text_item_range"),
                    )?;
                    Self::validate_optional_confidence(
                        span.confidence,
                        &format!("{span_path}.confidence"),
                    )?;
                }
            }
            // Reuse Block's streaming comparison after validating every source Line so
            // label-aware separators cannot drift between construction and validation.
            if !block.text_matches_lines() {
                return Err(Self::invalid(
                    format!("{block_path}.text"),
                    "must match the label-aware ordered Line projection",
                ));
            }
        }
        // Partial intersections are valid regardless of IoU and reported by the page analyzer.
        // Only full containment should have merged; detached annotations are exempt.
        for (index, block) in page
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| !block.is_detached())
        {
            for other in page
                .blocks
                .iter()
                .skip(index + 1)
                .filter(|block| !block.is_detached())
            {
                if block.bbox.contains_bbox(other.bbox)
                    || other.bbox.contains_bbox(block.bbox)
                {
                    tracing::error!(
                        "page {} content layouts {} and {} still satisfy the merge criteria",
                        page.page_number,
                        block.id.as_str(),
                        other.id.as_str()
                    );
                    return Err(Self::invalid(
                        format!("{path}.blocks[{index}].bbox"),
                        format!(
                            "content layout must be merged with {}",
                            other.id.as_str()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validates all relation endpoints after collecting the complete ID sets.
    fn validate_relations(
        document: &DocumentResult,
        block_ids: &BTreeSet<String>,
        line_ids: &BTreeSet<String>,
    ) -> Result<(), ValidationError> {
        for (index, relation) in document.relations.relations.iter().enumerate()
        {
            let path = format!("relations[{index}]");
            Self::validate_node_ref(
                &relation.source,
                &format!("{path}.source"),
                block_ids,
                line_ids,
            )?;
            Self::validate_node_ref(
                &relation.target,
                &format!("{path}.target"),
                block_ids,
                line_ids,
            )?;
            Self::validate_optional_confidence(
                relation.score,
                &format!("{path}.score"),
            )?;
        }
        Ok(())
    }

    /// Validates one non-owning relation endpoint against known IDs.
    fn validate_node_ref(
        reference: &NodeRef,
        path: &str,
        block_ids: &BTreeSet<String>,
        line_ids: &BTreeSet<String>,
    ) -> Result<(), ValidationError> {
        Self::validate_id_page(
            reference.block_id.as_str(),
            reference.page_number,
            path,
        )?;
        if !block_ids.contains(reference.block_id.as_str()) {
            return Err(Self::invalid(
                path,
                "relation references an unknown BlockId",
            ));
        }
        if let Some(line_id) = &reference.line_id {
            Self::validate_id_page(
                line_id.as_str(),
                reference.page_number,
                path,
            )?;
            if !line_ids.contains(line_id.as_str()) {
                return Err(Self::invalid(
                    path,
                    "relation references an unknown LineId",
                ));
            }
        }
        Ok(())
    }

    /// Validates one ID's encoded page number against its owning node.
    fn validate_id_page(
        id: &str,
        expected_page: u32,
        path: &str,
    ) -> Result<(), ValidationError> {
        let page = id
            .strip_prefix('p')
            .and_then(|value| value.split(':').next())
            .and_then(|value| value.parse::<u32>().ok());
        if page == Some(expected_page) {
            Ok(())
        } else {
            Err(Self::invalid(
                path,
                format!(
                    "ID page {page:?} does not match owner page {expected_page}"
                ),
            ))
        }
    }

    /// Validates finite positive box coordinates even if an unchecked builder was used.
    fn validate_bbox(bbox: Bbox, path: &str) -> Result<(), ValidationError> {
        let finite = [bbox.left, bbox.top, bbox.right, bbox.bottom]
            .into_iter()
            .all(f64::is_finite);
        if finite && bbox.right > bbox.left && bbox.bottom > bbox.top {
            Ok(())
        } else {
            Err(Self::invalid(
                path,
                "bbox must be finite with positive area",
            ))
        }
    }

    /// Returns whether an inner box fits inside an outer box with float tolerance.
    fn bbox_contains(outer: Bbox, inner: Bbox) -> bool {
        inner.left >= outer.left - GEOMETRY_TOLERANCE
            && inner.top >= outer.top - GEOMETRY_TOLERANCE
            && inner.right <= outer.right + GEOMETRY_TOLERANCE
            && inner.bottom <= outer.bottom + GEOMETRY_TOLERANCE
    }

    /// Validates an optional probability-like value.
    fn validate_optional_confidence(
        value: Option<f64>,
        path: &str,
    ) -> Result<(), ValidationError> {
        if value.is_none_or(|value| {
            value.is_finite() && (0.0..=1.0).contains(&value)
        }) {
            Ok(())
        } else {
            Err(Self::invalid(path, "must be finite and within [0, 1]"))
        }
    }

    /// Validates one half-open inline text-item range.
    fn validate_text_range(
        range: TextItemRange,
        item_count: usize,
        path: &str,
    ) -> Result<(), ValidationError> {
        if range.start <= range.end && range.end <= item_count {
            Ok(())
        } else {
            Err(Self::invalid(
                path,
                format!(
                    "range {}..{} exceeds {item_count} text items",
                    range.start, range.end
                ),
            ))
        }
    }

    /// Constructs one stable path-aware validation error.
    fn invalid(
        path: impl Into<String>,
        reason: impl Into<String>,
    ) -> ValidationError {
        ValidationError::InvalidNode {
            path: path.into(),
            reason: reason.into(),
        }
    }
}
