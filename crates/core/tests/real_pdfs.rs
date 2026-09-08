mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use common::e2e_manifest::{load_manifest, verify_pdf_directory};
use docparse_config::{ConfigLoader, ValidatedConfig};
use docparse_core::{
    DocParser, DocumentResult, InlineContentStatus, LabelSource,
    ResultValidator, TextSource, write_pdf_overlays_for_pages,
};
use sha2::{Digest, Sha256};

type PageMetric = (String, u32, f64, u64, u64);

/// Streaming writer that compares serialized bytes against one canonical buffer.
struct ExactJsonComparison<'a> {
    expected: &'a [u8],
    position: usize,
    exact: bool,
}

impl<'a> ExactJsonComparison<'a> {
    /// Creates a comparison writer positioned at the first expected byte.
    const fn new(expected: &'a [u8]) -> Self {
        Self {
            expected,
            position: 0,
            exact: true,
        }
    }

    /// Returns whether every expected byte was written exactly once in order.
    fn is_exact(&self) -> bool {
        self.exact && self.position == self.expected.len()
    }
}

impl Write for ExactJsonComparison<'_> {
    /// Consumes serialized bytes while recording any content or length mismatch.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let end = self.position.checked_add(buffer.len()).ok_or_else(|| {
            io::Error::other("canonical JSON comparison length overflow")
        })?;
        self.exact &= self.expected.get(self.position..end) == Some(buffer);
        self.position = end;
        Ok(buffer.len())
    }

    /// Performs no work because comparison has no buffered external resource.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Verifies streaming canonical comparison detects byte and length mismatches.
#[test]
fn exact_json_comparison_requires_identical_bytes() {
    let mut matching = ExactJsonComparison::new(b"canonical");
    std::io::Write::write_all(&mut matching, b"canon")
        .expect("comparison writer must accept the prefix");
    std::io::Write::write_all(&mut matching, b"ical")
        .expect("comparison writer must accept the suffix");
    assert!(matching.is_exact());

    let mut different = ExactJsonComparison::new(b"canonical");
    std::io::Write::write_all(&mut different, b"canonXcal")
        .expect("comparison writer must consume mismatched bytes");
    assert!(!different.is_exact());

    let mut short = ExactJsonComparison::new(b"canonical");
    std::io::Write::write_all(&mut short, b"canon")
        .expect("comparison writer must consume short bytes");
    assert!(!short.is_exact());
}

/// Runs the fixed real corpus through the production parser and writes deterministic artifacts.
#[tokio::test]
#[ignore = "requires fixed PP-DocLayoutV3 model and local E2E corpus"]
async fn real_pdf_corpus() -> Result<(), Box<dyn Error>> {
    let pdf_dir = required_env_path("DOCPARSE_E2E_PDF_DIR")?;
    let manifest_path = required_env_path("DOCPARSE_E2E_MANIFEST")?;
    let output_dir = required_env_path("DOCPARSE_E2E_OUTPUT_DIR")?;
    let config_path = required_env_path("DOCPARSE_E2E_CONFIG")?;
    let manifest = load_manifest(&manifest_path)?;
    let verified = verify_pdf_directory(&manifest, &pdf_dir)?;
    let raw_config = ConfigLoader::new(&config_path).load_raw()?;
    let config_identity = serde_json::json!({
        "layout": {
            "score_threshold": raw_config.layout.score_threshold,
            "execution_provider": raw_config.layout.execution_provider,
        },
        "render": raw_config.render,
        "fusion": raw_config.fusion,
        "ocr": raw_config.ocr,
        "output": raw_config.output,
    });
    let config_fingerprint =
        sha256_bytes(&serde_json::to_vec(&config_identity)?);
    let model_manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&raw_config.layout.model_manifest_path)?,
    )?;
    let model_revision = model_manifest
        .get("revision")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            std::io::Error::other("model manifest has no revision")
        })?;
    let model_sha256 = model_manifest
        .get("files")
        .and_then(|files| files.get("inference.onnx"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            std::io::Error::other("model manifest has no ONNX hash")
        })?;
    let config = Arc::new(ValidatedConfig::try_from(raw_config)?);
    let parser = DocParser::builder()
        .config(Arc::clone(&config))
        .build()
        .await?;
    let selected = std::env::var("DOCPARSE_E2E_ONLY").ok();
    if let Some(logical_id) = &selected
        && !verified
            .iter()
            .any(|document| &document.manifest.logical_id == logical_id)
    {
        return Err(std::io::Error::other(format!(
            "unknown DOCPARSE_E2E_ONLY logical ID {logical_id}"
        ))
        .into());
    }

    let documents_dir = output_dir.join("documents");
    std::fs::create_dir_all(&documents_dir)?;
    let mut canonical_documents = Vec::new();
    let mut totals = BTreeMap::<String, u64>::new();
    let mut label_counts = BTreeMap::<String, u64>::new();
    let mut warning_counts = BTreeMap::<String, u64>::new();
    let mut page_metrics = Vec::<PageMetric>::new();
    for document in verified.iter().filter(|document| {
        selected.as_ref().is_none_or(|logical_id| {
            logical_id == &document.manifest.logical_id
        })
    }) {
        eprintln!("running real E2E for {}", document.manifest.logical_id);
        let result =
            parser.parse_path(&document.path).await.map_err(|error| {
                std::io::Error::other(format!(
                    "{} parse failed: {error}",
                    document.manifest.logical_id
                ))
            })?;
        if result.pages.len()
            != usize::try_from(document.manifest.page_count)
                .unwrap_or(usize::MAX)
        {
            return Err(invariant_error(
                &document.manifest.logical_id,
                0,
                "page_count",
                format!(
                    "expected {}, got {}",
                    document.manifest.page_count,
                    result.pages.len()
                ),
            )
            .into());
        }
        ResultValidator::validate(&result).map_err(|error| {
            invariant_error(
                &document.manifest.logical_id,
                0,
                "result_validator",
                error.to_string(),
            )
        })?;
        check_document_invariants(&document.manifest.logical_id, &result)?;
        let page_hashes: Vec<_> = result
            .pages
            .iter()
            .map(|page| {
                Ok::<_, serde_json::Error>(serde_json::json!({
                    "page_number": page.page_number,
                    "sha256": sha256_bytes(&serde_json::to_vec(page)?),
                    "blocks": page.blocks.len(),
                    "lines": page.iter_lines().count(),
                    "text_items": page.iter_text_items().count(),
                }))
            })
            .collect::<Result<_, _>>()?;
        accumulate_counts(
            &result,
            &mut totals,
            &mut label_counts,
            &mut warning_counts,
        );
        collect_page_metrics(
            &document.manifest.logical_id,
            &result,
            &mut page_metrics,
        );
        let page_count = result.pages.len();
        let canonical = serde_json::to_vec(&result)?;
        let document_sha256 = sha256_bytes(&canonical);
        // The decoded value replaces the original before round-trip verification so the E2E
        // memory metric does not retain two complete object graphs at once.
        drop(result);
        let decoded: DocumentResult = serde_json::from_slice(&canonical)?;
        ResultValidator::validate(&decoded)?;
        // Canonical byte equality is the externally observable round-trip contract.
        let mut comparison = ExactJsonComparison::new(&canonical);
        serde_json::to_writer(&mut comparison, &decoded)?;
        if !comparison.is_exact() {
            std::fs::write(
                output_dir.join("roundtrip-first.json"),
                &canonical,
            )?;
            std::fs::write(
                output_dir.join("roundtrip-second.json"),
                serde_json::to_vec(&decoded)?,
            )?;
            return Err(invariant_error(
                &document.manifest.logical_id,
                0,
                "json_round_trip",
                "decoded document changes canonical JSON bytes",
            )
            .into());
        }
        let pretty_path = documents_dir
            .join(format!("{}.json", document.manifest.logical_id));
        let mut pretty_writer = BufWriter::new(File::create(pretty_path)?);
        serde_json::to_writer_pretty(&mut pretty_writer, &decoded)?;
        pretty_writer.flush()?;
        canonical_documents.push(serde_json::json!({
            "logical_id": document.manifest.logical_id,
            "document_sha256": document_sha256,
            "page_count": page_count,
            "pages": page_hashes,
        }));
    }
    canonical_documents.sort_by(|left, right| {
        left.get("logical_id")
            .and_then(serde_json::Value::as_str)
            .cmp(&right.get("logical_id").and_then(serde_json::Value::as_str))
    });
    totals.insert(
        "documents".to_owned(),
        u64::try_from(canonical_documents.len()).unwrap_or(u64::MAX),
    );
    let canonical_hashes = serde_json::json!({
        "schema_version": 1,
        "corpus_manifest_sha256": sha256_bytes(&std::fs::read(&manifest_path)?),
        "model_revision": model_revision,
        "model_sha256": model_sha256,
        "config_fingerprint": config_fingerprint,
        "documents": canonical_documents,
    });
    let summary = serde_json::json!({
        "schema_version": 1,
        "counts": totals,
        "labels": label_counts,
        "warnings": warning_counts,
        "invariant_failures": 0,
    });
    std::fs::write(
        output_dir.join("canonical-hashes.json"),
        serde_json::to_vec_pretty(&canonical_hashes)?,
    )?;
    std::fs::write(
        output_dir.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    if std::env::var("DOCPARSE_E2E_WRITE_OVERLAYS").as_deref() == Ok("1") {
        write_selected_overlays(
            config.as_ref(),
            &verified,
            selected.as_deref(),
            &documents_dir,
            &output_dir,
            &page_metrics,
        )
        .await?;
    }
    Ok(())
}

/// Reads one mandatory runner-provided filesystem path or returns the invocation hint.
fn required_env_path(name: &str) -> Result<PathBuf, std::io::Error> {
    std::env::var_os(name).map(PathBuf::from).ok_or_else(|| {
        std::io::Error::other(format!(
            "missing {name}; run `uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3`"
        ))
    })
}

/// Checks every page's geometry, identity ownership, order, and inline formula contract.
fn check_document_invariants(
    logical_id: &str,
    result: &DocumentResult,
) -> Result<(), std::io::Error> {
    let mut document_text_ids = BTreeSet::new();
    for page in &result.pages {
        if page.page_number == 0
            || !page.width.is_finite()
            || page.width <= 0.0
            || !page.height.is_finite()
            || page.height <= 0.0
            || !matches!(page.rotation, 0 | 90 | 180 | 270)
        {
            return Err(invariant_error(
                logical_id,
                page.page_number,
                "page_geometry",
                "invalid page dimensions or rotation",
            ));
        }
        for (block_index, block) in page.blocks.iter().enumerate() {
            if usize::try_from(block.final_order).unwrap_or(usize::MAX)
                != block_index
            {
                return Err(invariant_error(
                    logical_id,
                    page.page_number,
                    "final_order",
                    format!(
                        "block {} is at index {block_index}",
                        block.id.as_str()
                    ),
                ));
            }
            check_bbox(logical_id, page, block.bbox, "block_bbox")?;
            for line in &block.lines {
                check_bbox(logical_id, page, line.bbox, "line_bbox")?;
                for item in &line.text_items {
                    check_bbox(logical_id, page, item.bbox, "text_item_bbox")?;
                    if !document_text_ids.insert(item.id.as_str().to_owned()) {
                        return Err(invariant_error(
                            logical_id,
                            page.page_number,
                            "unique_text_owner",
                            format!("duplicate {}", item.id.as_str()),
                        ));
                    }
                    if item.source == TextSource::Native
                        && !item
                            .id
                            .as_str()
                            .starts_with(&format!("p{}:t", page.page_number))
                    {
                        return Err(invariant_error(
                            logical_id,
                            page.page_number,
                            "native_text_id",
                            item.id.as_str(),
                        ));
                    }
                }
                for span in &line.inline_spans {
                    if span.text_item_range.start > span.text_item_range.end
                        || span.text_item_range.end > line.text_items.len()
                        || (span.content_status == InlineContentStatus::Missing
                            && span.extracted_text.is_some())
                    {
                        return Err(invariant_error(
                            logical_id,
                            page.page_number,
                            "inline_span",
                            line.id.as_str(),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Checks one result box against finite positive geometry and tolerant page bounds.
fn check_bbox(
    logical_id: &str,
    page: &docparse_core::PageResult,
    bbox: docparse_layout::Bbox,
    invariant: &str,
) -> Result<(), std::io::Error> {
    const PAGE_TOLERANCE: f64 = 2.0;
    let valid = [bbox.left, bbox.top, bbox.right, bbox.bottom]
        .into_iter()
        .all(f64::is_finite)
        && bbox.right > bbox.left
        && bbox.bottom > bbox.top
        && bbox.left >= -PAGE_TOLERANCE
        && bbox.top >= -PAGE_TOLERANCE
        && bbox.right <= page.width + PAGE_TOLERANCE
        && bbox.bottom <= page.height + PAGE_TOLERANCE;
    if valid {
        Ok(())
    } else {
        Err(invariant_error(
            logical_id,
            page.page_number,
            invariant,
            format!("bbox={bbox:?}, page={}x{}", page.width, page.height),
        ))
    }
}

/// Adds deterministic document counts to corpus-level maps.
fn accumulate_counts(
    result: &DocumentResult,
    totals: &mut BTreeMap<String, u64>,
    labels: &mut BTreeMap<String, u64>,
    warnings: &mut BTreeMap<String, u64>,
) {
    *totals.entry("pages".to_owned()).or_default() +=
        u64::try_from(result.pages.len()).unwrap_or(u64::MAX);
    *totals.entry("blocks".to_owned()).or_default() += u64::try_from(
        result
            .pages
            .iter()
            .map(|page| page.blocks.len())
            .sum::<usize>(),
    )
    .unwrap_or(u64::MAX);
    *totals.entry("lines".to_owned()).or_default() += u64::try_from(
        result
            .pages
            .iter()
            .map(|page| page.iter_lines().count())
            .sum::<usize>(),
    )
    .unwrap_or(u64::MAX);
    *totals.entry("text_items".to_owned()).or_default() += u64::try_from(
        result
            .pages
            .iter()
            .map(|page| page.iter_text_items().count())
            .sum::<usize>(),
    )
    .unwrap_or(u64::MAX);
    for block in result.pages.iter().flat_map(|page| &page.blocks) {
        let label = serde_json::to_value(&block.label)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        *labels.entry(label).or_default() += 1;
        let key = match block.label_source {
            LabelSource::Model => "model_blocks",
            LabelSource::Fallback => "fallback_blocks",
            LabelSource::Heuristic => "heuristic_blocks",
            LabelSource::Pdf => "pdf_blocks",
        };
        *totals.entry(key.to_owned()).or_default() += 1;
    }
    for warning in result.pages.iter().flat_map(|page| &page.warnings) {
        *warnings.entry(warning.code.clone()).or_default() += 1;
    }
    *totals.entry("removed_order_edges".to_owned()).or_default() += result
        .pages
        .iter()
        .map(|page| {
            page.diagnostics
                .keys()
                .filter(|key| key.starts_with("order.removed_edge."))
                .count() as u64
        })
        .sum::<u64>();
    *totals.entry("errors".to_owned()).or_default() +=
        u64::try_from(result.errors.len()).unwrap_or(u64::MAX);
}

/// Records deterministic fallback, assignment-conflict, and removed-edge page metrics.
fn collect_page_metrics(
    logical_id: &str,
    result: &DocumentResult,
    metrics: &mut Vec<PageMetric>,
) {
    for page in &result.pages {
        let fallback_blocks = page
            .blocks
            .iter()
            .filter(|block| block.label_source == LabelSource::Fallback)
            .count();
        let fallback_ratio = if page.blocks.is_empty() {
            0.0
        } else {
            fallback_blocks as f64 / page.blocks.len() as f64
        };
        let assignment_conflicts = page
            .blocks
            .iter()
            .flat_map(|block| &block.evidence)
            .filter(|evidence| {
                evidence.kind == "primary_assignment"
                    && evidence
                        .details
                        .get("alternatives")
                        .and_then(|value| value.parse::<u64>().ok())
                        .is_some_and(|count| count > 0)
            })
            .count();
        let removed_edges = page
            .diagnostics
            .keys()
            .filter(|key| key.starts_with("order.removed_edge."))
            .count();
        metrics.push((
            logical_id.to_owned(),
            page.page_number,
            fallback_ratio,
            u64::try_from(assignment_conflicts).unwrap_or(u64::MAX),
            u64::try_from(removed_edges).unwrap_or(u64::MAX),
        ));
    }
}

/// Selects representative pages and writes only their model-independent overlays.
async fn write_selected_overlays(
    config: &ValidatedConfig,
    verified: &[common::e2e_manifest::VerifiedE2eDocument],
    selected_logical_id: Option<&str>,
    documents_dir: &std::path::Path,
    output_dir: &std::path::Path,
    metrics: &[PageMetric],
) -> Result<(), Box<dyn Error>> {
    let mut selection = BTreeMap::<(String, u32), BTreeSet<String>>::new();
    for document in verified.iter().filter(|document| {
        selected_logical_id
            .is_none_or(|logical_id| logical_id == document.manifest.logical_id)
    }) {
        for (page_number, reason) in [
            (1, "first"),
            (document.manifest.page_count.div_ceil(2), "middle"),
            (document.manifest.page_count, "last"),
        ] {
            selection
                .entry((document.manifest.logical_id.clone(), page_number))
                .or_default()
                .insert(reason.to_owned());
        }
    }
    for (metric_index, reason) in [
        (2, "max_fallback_ratio"),
        (3, "max_assignment_conflicts"),
        (4, "max_removed_edges"),
    ] {
        if let Some(metric) = maximum_metric(metrics, metric_index) {
            selection
                .entry((metric.0.clone(), metric.1))
                .or_default()
                .insert(reason.to_owned());
        }
    }

    let overlays_dir = output_dir.join("overlays");
    for document in verified.iter().filter(|document| {
        selection
            .keys()
            .any(|(logical_id, _)| logical_id == &document.manifest.logical_id)
    }) {
        let result: DocumentResult = serde_json::from_slice(&std::fs::read(
            documents_dir
                .join(format!("{}.json", document.manifest.logical_id)),
        )?)?;
        let pages: BTreeSet<_> = selection
            .keys()
            .filter(|(logical_id, _)| {
                logical_id == &document.manifest.logical_id
            })
            .map(|(_, page_number)| *page_number)
            .collect();
        write_pdf_overlays_for_pages(
            config,
            &document.path,
            &result,
            &overlays_dir.join(&document.manifest.logical_id),
            Some(&pages),
        )
        .await?;
    }
    let entries: Vec<_> = selection
        .into_iter()
        .map(|((logical_id, page_number), reasons)| {
            let metric = metrics.iter().find(|metric| {
                metric.0 == logical_id && metric.1 == page_number
            });
            serde_json::json!({
                "logical_id": logical_id,
                "page_number": page_number,
                "reasons": reasons,
                "fallback_ratio": metric.map_or(0.0, |metric| metric.2),
                "assignment_conflicts": metric.map_or(0, |metric| metric.3),
                "removed_edges": metric.map_or(0, |metric| metric.4),
            })
        })
        .collect();
    std::fs::write(
        output_dir.join("overlay-selection.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "pages": entries,
        }))?,
    )?;
    Ok(())
}

/// Returns the stable highest page for one metric tuple position.
fn maximum_metric(
    metrics: &[PageMetric],
    metric_index: usize,
) -> Option<&PageMetric> {
    metrics.iter().max_by(|left, right| {
        let value_order = match metric_index {
            2 => left.2.total_cmp(&right.2),
            3 => left.3.cmp(&right.3),
            4 => left.4.cmp(&right.4),
            _ => std::cmp::Ordering::Equal,
        };
        value_order
            .then_with(|| right.0.cmp(&left.0))
            .then_with(|| right.1.cmp(&left.1))
    })
}

/// Creates one stable, context-rich invariant failure without embedding PDF text.
fn invariant_error(
    logical_id: &str,
    page_number: u32,
    invariant: &str,
    detail: impl std::fmt::Display,
) -> std::io::Error {
    std::io::Error::other(format!(
        "logical_id={logical_id} page={page_number} invariant={invariant}: {detail}"
    ))
}

/// Hashes deterministic bytes into lowercase SHA-256.
fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        for nibble in [byte >> 4, byte & 0x0f] {
            let digit = match nibble {
                0..=9 => b'0' + nibble,
                _ => b'a' + (nibble - 10),
            };
            encoded.push(char::from(digit));
        }
    }
    encoded
}
