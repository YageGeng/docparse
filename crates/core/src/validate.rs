use std::collections::BTreeSet;

use docparse_layout::Bbox;

use crate::{
    Block, DocumentResult, NodeRef, PageResult, TextItemRange, ValidationError,
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

    /// Checks list references and marker boundaries without treating annotations as additional text owners.
    fn validate_block_lists(
        block: &Block,
        path: &str,
    ) -> Result<(), ValidationError> {
        if block.list_items.is_empty() {
            return Ok(());
        }
        let invalid = |message: &str| {
            Self::invalid(format!("{path}.list_items"), message)
        };
        if block.label != docparse_layout::LayoutLabel::Text
            || block.table.is_some()
        {
            return Err(invalid(
                "only body Text layouts may contain recovered lists",
            ));
        }
        let lines: std::collections::BTreeMap<_, _> = block
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| (&line.id, (index, line)))
            .collect();
        let mut previous_group = None;
        let mut previous_start = None;
        let mut owners = vec![None; block.lines.len()];
        for (item_index, item) in block.list_items.iter().enumerate() {
            if previous_group != Some(item.group)
                && (item.level != 0
                    || previous_group.is_some_and(|group| item.group <= group))
            {
                return Err(invalid(
                    "list groups must be ordered and start at level zero",
                ));
            }
            if (item.kind == crate::ListKind::Unordered)
                != item.ordinal.is_none()
            {
                return Err(invalid(
                    "ordered markers need an ordinal; unordered markers must not have one",
                ));
            }
            let first = item
                .line_ids
                .first()
                .and_then(|id| lines.get(id))
                .map(|(_, line)| *line)
                .ok_or_else(|| {
                    invalid("list item needs a source line in its own block")
                })?;
            let Some(prefix) = first.text.get(..item.marker_end) else {
                return Err(invalid(
                    "marker prefix must end on a valid UTF-8 source boundary",
                ));
            };
            if item.marker.trim().is_empty()
                || prefix.trim() != item.marker
                || item.marker_end >= first.text.len()
                || !item.matches_source(first)
            {
                return Err(invalid(
                    "marker prefix must match the source and leave a nonempty body",
                ));
            }
            let mut previous_line = None;
            for (part, id) in item.line_ids.iter().enumerate() {
                let (index, _) = lines.get(id).ok_or_else(|| {
                    invalid("list line belongs to a different block")
                })?;
                if previous_line.is_some_and(|previous| *index <= previous)
                    || part == 0
                        && previous_start
                            .is_some_and(|previous| *index <= previous)
                {
                    return Err(invalid(
                        "list items and their lines must follow source order",
                    ));
                }
                let owner =
                    owners.get_mut(*index).expect("indexed source line");
                if owner.replace((item_index, part)).is_some() {
                    return Err(invalid(
                        "list lines must be uniquely referenced",
                    ));
                }
                if part == 0 {
                    previous_start = Some(*index);
                }
                previous_line = Some(*index);
            }
            previous_group = Some(item.group);
        }
        // A continuation may skip descendants, but cannot cross a sibling, another group, or unrelated prose.
        let mut active = Vec::new();
        let mut group = None;
        for owner in owners {
            let Some((item_index, part)) = owner else {
                active.clear();
                continue;
            };
            let item =
                block.list_items.get(item_index).expect("indexed list item");
            if part == 0 {
                if group != Some(item.group) {
                    active.clear();
                } else if active.is_empty() {
                    return Err(invalid(
                        "a list group cannot cross unrelated prose",
                    ));
                }
                if item.level as usize > active.len() {
                    return Err(invalid(
                        "list nesting cannot skip an active parent",
                    ));
                }
                active.truncate(item.level as usize);
                active.push(item_index);
                group = Some(item.group);
            } else {
                let Some(level) =
                    active.iter().position(|active| *active == item_index)
                else {
                    return Err(invalid(
                        "list continuation must belong to an active ancestor",
                    ));
                };
                active.truncate(level + 1);
            }
        }
        Ok(())
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

        // Validate unmatched page images with the same content rules as block-owned images.
        let mut image_ids = BTreeSet::new();
        for (index, asset) in page.images.iter().enumerate() {
            let image_path = format!("{path}.images[{index}]");
            if asset.id.is_empty() || !image_ids.insert(&asset.id) {
                return Err(Self::invalid(
                    &image_path,
                    "image IDs must be non-empty and unique",
                ));
            }
            Self::validate_bbox(asset.bbox, &format!("{image_path}.bbox"))?;
        }
        for (image, image_path) in page
            .images
            .iter()
            .enumerate()
            .map(|(index, asset)| {
                (&asset.image, format!("{path}.images[{index}].image"))
            })
            .chain(page.blocks.iter().enumerate().filter_map(
                |(index, block)| {
                    block.image.as_ref().map(|image| {
                        (image, format!("{path}.blocks[{index}].image"))
                    })
                },
            ))
        {
            if image.width == 0 || image.height == 0 {
                return Err(Self::invalid(
                    &image_path,
                    "width and height must be positive",
                ));
            }
            match &image.delivery {
                crate::FigureDelivery::File { path }
                    if path.is_empty()
                        || !std::path::Path::new(path).is_absolute() =>
                {
                    return Err(Self::invalid(
                        format!("{image_path}.delivery.path"),
                        "must be a non-empty absolute path",
                    ));
                }
                crate::FigureDelivery::Inline { data_base64 }
                    if data_base64.is_empty() =>
                {
                    return Err(Self::invalid(
                        format!("{image_path}.delivery.data_base64"),
                        "must be non-empty",
                    ));
                }
                crate::FigureDelivery::File { .. }
                | crate::FigureDelivery::Inline { .. } => {}
            }
        }

        let mut formula_ids = BTreeSet::new();
        for (index, formula) in page.formulas.iter().enumerate() {
            let location = format!("{path}.formulas[{index}]");
            Self::validate_id_page(
                formula.id.as_str(),
                page.page_number,
                &location,
            )?;
            Self::validate_bbox(formula.bbox, &location)?;
            if let Some(crop) = formula.crop_bbox {
                Self::validate_bbox(crop, &location)?;
                if !crop.contains_bbox(formula.bbox) {
                    return Err(Self::invalid(
                        &location,
                        "formula crop must contain its original detection box",
                    ));
                }
            }
            if !formula_ids.insert(formula.id.as_str())
                || !matches!(
                    formula.label,
                    docparse_layout::LayoutLabel::InlineFormula
                        | docparse_layout::LayoutLabel::DisplayFormula
                )
            {
                return Err(Self::invalid(
                    &location,
                    "formula identity or label is invalid",
                ));
            }
            match (&formula.latex, &formula.markdown, &formula.error) {
                (Some(latex), Some(markdown), None)
                    if !latex.trim().is_empty() =>
                {
                    let expected = if formula.label
                        == docparse_layout::LayoutLabel::InlineFormula
                    {
                        format!("${latex}$")
                    } else {
                        format!("$$\n{latex}\n$$")
                    };
                    if *markdown != expected {
                        return Err(Self::invalid(
                            &location,
                            "formula LaTeX and Markdown disagree",
                        ));
                    }
                }
                (None, None, Some(error)) if !error.is_empty() => {}
                _ => {
                    return Err(Self::invalid(
                        &location,
                        "formula must contain both representations or an explicit failure",
                    ));
                }
            }
            let block = formula.block_id.as_ref().and_then(|id| {
                page.blocks.iter().find(|block| &block.id == id)
            });
            for span in &formula.text_spans {
                let item = block
                    .into_iter()
                    .flat_map(|block| &block.lines)
                    .flat_map(|line| &line.text_items)
                    .find(|item| item.id == span.text_item_id)
                    .ok_or_else(|| {
                        Self::invalid(
                            &location,
                            "formula source item is missing",
                        )
                    })?;
                if span.byte_range.is_empty()
                    || item.raw_text.get(span.byte_range.clone()).is_none()
                {
                    return Err(Self::invalid(
                        &location,
                        "formula source bytes are invalid",
                    ));
                }
                Self::validate_bbox(span.bbox, &location)?;
            }
            if formula.block_id.is_some() && block.is_none() {
                return Err(Self::invalid(
                    &location,
                    "formula block reference is missing",
                ));
            }
            match (
                &formula.line_id,
                formula.text_item_range,
                formula.table_cell,
            ) {
                (Some(id), Some(range), None) => {
                    let line = block
                        .and_then(|block| {
                            block.lines.iter().find(|line| &line.id == id)
                        })
                        .ok_or_else(|| {
                            Self::invalid(
                                &location,
                                "formula line reference is missing",
                            )
                        })?;
                    if range.start > range.end
                        || range.end > line.text_items.len()
                    {
                        return Err(Self::invalid(
                            &location,
                            "formula text range exceeds its source line",
                        ));
                    }
                }
                (None, None, Some((row, column))) => {
                    if !block
                        .and_then(|block| block.table.as_ref())
                        .is_some_and(|table| {
                            table.cells.iter().any(|cell| {
                                cell.row == row && cell.column == column
                            })
                        })
                    {
                        return Err(Self::invalid(
                            &location,
                            "formula cell reference is missing",
                        ));
                    }
                }
                (None, None, None) => {}
                _ => {
                    return Err(Self::invalid(
                        &location,
                        "formula anchors are inconsistent",
                    ));
                }
            }
        }
        // Replaced mapping failures still own their original IDs outside canonical reading order.
        for (index, item) in page.replaced_native_text.iter().enumerate() {
            let item_path = format!("{path}.replaced_native_text[{index}]");
            Self::validate_id_page(
                item.id.as_str(),
                page.page_number,
                &item_path,
            )?;
            if item.source != crate::TextSource::Native
                || item.watermark.is_some()
                || !text_item_ids.insert(item.id.as_str().to_owned())
            {
                return Err(Self::invalid(
                    &item_path,
                    "archive must uniquely own non-watermark native facts",
                ));
            }
            Self::validate_bbox(item.bbox, &item_path)?;
            Self::validate_optional_confidence(item.confidence, &item_path)?;
            if !item.rotation.is_finite()
                || item
                    .polygon
                    .as_ref()
                    .is_some_and(|p| !Self::bbox_contains(item.bbox, p.bbox()))
            {
                return Err(Self::invalid(
                    &item_path,
                    "invalid archived text geometry",
                ));
            }
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

            Self::validate_block_lists(block, &block_path)?;
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
            if let Some(table) = &block.table {
                table.validate(block).map_err(|reason| {
                    Self::invalid(format!("{block_path}.table"), reason)
                })?;
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
                // Sparse unions can contain independent content in empty corners,
                // including after optional evidence is hidden from browser JSON.
                let sparse = |candidate: &Block| {
                    candidate
                        .semantic_hints
                        .get(Block::SPARSE_LAYOUT_HINT)
                        .is_some_and(|value| value == "true")
                };
                if (block.bbox.contains_bbox(other.bbox)
                    || other.bbox.contains_bbox(block.bbox))
                    && !sparse(block)
                    && !sparse(other)
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
