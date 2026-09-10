/** An immutable canonical text fact; additional evidence follows the Rust schema. */
export interface TextItem { id: string; raw_text: string; [key: string]: unknown }
/** A canonical line in the parser's reading order. */
export interface Line { id: string; text: string; text_items: TextItem[]; [key: string]: unknown }
/** Canonical viewport bounds in PDF points. */
export interface Bbox { left: number; top: number; right: number; bottom: number }
/** A vertex in canonical viewport points, matching the page image's coordinate space. */
export interface Point { x: number; y: number }
/** Original region geometry retained when several candidates become one content layout. */
export interface SourceRegionEvidence { label?: string; model_region_id: string | null; fallback_region_id: string | null; bbox: Bbox; polygon: Point[] | null; geometry_source: string; confidence: number | null; model_order: number | null }
/** A layout block with its original nested text facts. */
export interface Block { id: string; label: string; text: string; bbox: Bbox; polygon: Point[] | null; source_region: SourceRegionEvidence | null; source_regions?: SourceRegionEvidence[]; final_order: number; lines: Line[]; table?: Table; [key: string]: unknown }
/** A non-owning source slice. Byte offsets address UTF-8, not JavaScript UTF-16 strings. */
export interface TableTextSpan { text_item_id: string; byte_range: { start: number; end: number }; bbox: Bbox }
/** Cell-local physical line with the original source references. */
export interface TableCellLine { text: string; bbox: Bbox; spans: TableTextSpan[] }
/** A zero-based logical cell; covered positions do not appear as duplicate cells. */
export interface TableCell { row: number; column: number; row_span: number; column_span: number; bbox: Bbox | null; is_header: boolean; text: string; lines: TableCellLine[] }
/** A recovered table view; original TextItems remain owned by the parent block's lines. */
export interface Table { row_count: number; column_count: number; cells: TableCell[]; source: "tagged_pdf" | "ruled" | "text_alignment" | "external_tsr" }
/** A canonical page with viewport coordinates and recoverable warnings. */
export interface PageResult { page_number: number; width: number; height: number; rotation: number; blocks: Block[]; warnings: PageWarning[]; diagnostics: Record<string, string> }
/** A recoverable stage failure or quality warning emitted by the actual parser. */
export interface PageWarning { code: string; stage: string; message: string }
/** The same JSON-compatible document aggregate emitted by native DocParse. */
export interface DocumentResult {
  schema_version: string;
  context: { page_count: number; model_revision: string | null; [key: string]: unknown };
  pages: PageResult[];
  relations: { relations: unknown[] };
  errors: { page_number: number; stage: string; code: string; message: string }[];
}

/** Caller-owned model bytes or URLs resolved against the calling page. */
export type ModelSource =
  | { kind: "urls"; model: string; config: string; manifest: string }
  | { kind: "bytes"; model: Uint8Array; config: Uint8Array; manifest: Uint8Array };

/** Business settings retain the native configuration's field names. */
export interface WebParseConfig {
  layout?: { score_threshold?: number; session_pool_size?: number };
  runtime?: { page_concurrency?: number; render_queue_capacity?: number; blocking_task_limit?: number; continue_on_page_error?: boolean };
  render?: { dpi?: number; max_long_edge_pixels?: number };
  fusion?: Partial<Record<"minimum_line_coverage" | "center_minimum_line_coverage" | "assignment_coverage_weight" | "assignment_center_weight" | "assignment_baseline_weight" | "assignment_confidence_weight" | "assignment_specificity_weight" | "paragraph_gap_multiplier" | "indent_tolerance_points" | "font_size_tolerance_points" | "estimated_font_size_tolerance_points", number>>;
  ocr?: { policy?: "disabled" | "missing_regions" };
  output?: { formula_placeholder?: string; include_evidence?: boolean; include_diagnostics?: boolean };
}

/** Layout inference backends; PDFium and text processing still run in WebAssembly. */
export type ExecutionProvider = "wasm" | "webgpu";

/** Browser runtime options, separate from the serializable Worker payload. */
export interface WebParserOptions {
  artifacts: ModelSource;
  runtimeBaseUrl?: string;
  /** Defaults to CPU/WASM for compatibility; request WebGPU to accelerate inference. */
  executionProvider?: ExecutionProvider;
  /** Allows CPU initialization only when the requested GPU is unavailable; defaults to false. */
  allowCpuFallback?: boolean;
  config?: WebParseConfig;
  signal?: AbortSignal;
  onProgress?: (progress: ParserProgress) => void;
  onTiming?: (timing: ParserTiming) => void;
}
/** Reported stages follow actual downloads, scanned pages, and completed analyses. */
export type ParserProgress =
  | { stage: "loading_runtime" | "initializing_model" | "opening" }
  | { stage: "downloading"; artifact: string; loaded: number; total?: number }
  | { stage: "scanning" | "analyzing"; completed: number; total: number }
  | { stage: "linking" | "complete"; total: number };
/** Elapsed wall time for an attempted stage, including errors; intervals may overlap or nest.
 * Page numbers are one-based; null denotes a document or initialization stage.
 * These observations never enter DocumentResult. A duration is not proof of stage success.
 */
export interface ParserTiming {
  stage: "runtime_load" | "model_download" | "model_init" | "pdf_open" | "text_extract" | "document_context" | "pdf_render" | "layout_preprocess" | "layout_queue" | "layout_inference" | "layout_readback" | "layout_postprocess" | "text_prepare" | "ocr" | "text_finish" | "table_structure" | "table_rules" | "table_external" | "table_fill" | "link_validate" | "parse_total" | "result_serialize" | "preview_encode" | "worker_total";
  page_number: number | null;
  duration_ms: number;
}
/** A PNG of the exact PDFium raster used for inference, without any overlay baked in. */
export interface PageImageResult { pageNumber: number; width: number; height: number; blob: Blob }
/** Structural recovery is scoped to regions already labeled table by layout. */
export type TableMode = "rules_only" | "fallback" | "external_only";
/** Per-parse external table limits; the default never calls a provider. */
export interface TableOptions { mode?: TableMode; max_in_flight?: number; timeout_ms?: number }
/** An affine map from crop pixels to canonical viewport points. */
export interface AffineTransform { a: number; b: number; c: number; d: number; e: number; f: number }
/** Owned crop sent to the caller's structure provider; one-based page numbers match DocumentResult. */
export interface TsrTableRequest {
  request_id: string;
  page_number: number;
  block_id: string;
  crop_bbox: Bbox;
  crop_to_viewport: AffineTransform;
  reason: { kind: "external_only" } | { kind: "rules_failed"; message: string };
  image: { width: number; height: number; blob: Blob };
}
/** Structure tokens paired one-to-one with cell boxes in the original request image's pixel space. */
export interface TsrTableInput { request_id: string; structure_tokens: string[]; cell_bboxes: number[][] }

/** Per-call cancellation and observations retained on the calling thread. */
export interface ParseOptions {
  table?: TableOptions;
  onTableStructure?: (request: TsrTableRequest, signal: AbortSignal) => Promise<TsrTableInput>;
  signal?: AbortSignal;
  onProgress?: (progress: ParserProgress) => void;
  onTiming?: (timing: ParserTiming) => void;
  onPageImage?: (image: PageImageResult) => void;
}
/** Supported projections of a canonical document. */
export type RenderFormat = "json" | "text" | "markdown";

/** Public asynchronous parser operations. */
export interface DocParser {
  /** The initialized layout backend, including any explicitly allowed CPU fallback. */
  readonly executionProvider: ExecutionProvider;
  /** Parses a copy of the caller's PDF bytes. */
  parse(pdf: Uint8Array, options?: ParseOptions): Promise<DocumentResult>;
  /** Renders a canonical result without rerunning extraction or inference. */
  render(document: DocumentResult, format: RenderFormat): Promise<string>;
  /** Stops pending work and releases the Worker. */
  close(): Promise<void>;
}
