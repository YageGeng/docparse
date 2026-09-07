/** An immutable canonical text fact; additional evidence follows the Rust schema. */
export interface TextItem { raw_text: string; [key: string]: unknown }
/** A canonical line in the parser's reading order. */
export interface Line { id: string; text: string; text_items: TextItem[]; [key: string]: unknown }
/** A layout block with its original nested text facts. */
export interface Block { id: string; label: string; text: string; lines: Line[]; [key: string]: unknown }
/** A canonical page with viewport coordinates and recoverable warnings. */
export interface PageResult { page_number: number; width: number; height: number; rotation: number; blocks: Block[]; warnings: unknown[]; diagnostics: Record<string, string> }
/** The same JSON-compatible document aggregate emitted by native DocParse. */
export interface DocumentResult {
  schema_version: string;
  context: { page_count: number; model_revision: string | null; [key: string]: unknown };
  pages: PageResult[];
  relations: { relations: unknown[] };
  errors: { page_number: number; stage: string; code: string; message: string }[];
}

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

export interface WebParserOptions {
  artifacts: ModelSource;
  runtimeBaseUrl?: string;
  executionProvider?: "wasm" | "webgpu";
  allowCpuFallback?: boolean;
  config?: WebParseConfig;
  signal?: AbortSignal;
}
export interface ParseOptions { signal?: AbortSignal }
export interface DocParser {
  parse(pdf: Uint8Array, options?: ParseOptions): Promise<DocumentResult>;
  render(document: DocumentResult, format: "json" | "text" | "markdown"): Promise<string>;
  close(): Promise<void>;
}

/** A request failure that preserves its stable machine-readable category. */
export class DocParseError extends Error {
  /** Creates a portable error without browser or Rust resource handles. */
  constructor(public readonly code: string, message: string) { super(message); this.name = "DocParseError"; }
}

type Pending = { id: number; resolve: (value: unknown) => void; reject: (error: Error) => void; cleanup: () => void; initializing: boolean };

/** Owns exactly one Worker and one outstanding request at a time. */
class WorkerParser implements DocParser {
  private readonly worker: Worker;
  private pending?: Pending;
  private nextId = 0;
  private state: "initializing" | "ready" | "busy" | "failed" | "closed" = "initializing";

  /** Installs fatal handlers before any initialization request is dispatched. */
  constructor() {
    this.worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module", name: "docparse" });
    this.worker.onerror = (event) => { event.preventDefault(); this.fail(new DocParseError("WorkerStopped", event.message || "DocParse Worker stopped"), "failed"); };
    this.worker.onmessageerror = () => this.fail(new DocParseError("WorkerStopped", "Worker response could not be decoded"), "failed");
    this.worker.onmessage = (event) => {
      const message = event.data;
      if (message?.fatal) { const error = new DocParseError(message.code || "WorkerStopped", message.message || "Worker failed"); if (message.stack) error.stack += `\nWorker cause: ${message.stack}`; this.fail(error, "failed"); return; }
      const pending = this.pending;
      if (!pending || message?.id !== pending.id) return;
      this.pending = undefined;
      pending.cleanup();
      if (message.ok) { this.state = "ready"; pending.resolve(message.value); }
      else {
        const error = new DocParseError(message.code || "OperationFailed", message.message || "DocParse operation failed");
        if (message.stack) error.stack += `\nWorker cause: ${message.stack}`;
        if (pending.initializing) { this.state = "failed"; this.worker.terminate(); }
        else this.state = "ready";
        pending.reject(error);
      }
    };
  }

  /** Transfers owned initialization data while retaining caller-owned buffers. */
  async initialize(options: WebParserOptions): Promise<void> {
    const transfers: Transferable[] = [];
    let artifacts: ModelSource;
    if (options.artifacts.kind === "urls") {
      artifacts = { kind: "urls", model: new URL(options.artifacts.model, location.href).href, config: new URL(options.artifacts.config, location.href).href, manifest: new URL(options.artifacts.manifest, location.href).href };
    } else {
      const model = new Uint8Array(options.artifacts.model);
      const config = new Uint8Array(options.artifacts.config);
      const manifest = new Uint8Array(options.artifacts.manifest);
      transfers.push(model.buffer, config.buffer, manifest.buffer);
      artifacts = { kind: "bytes", model, config, manifest };
    }
    const runtimeBase = options.runtimeBaseUrl ? new URL(options.runtimeBaseUrl, location.href) : undefined;
    if (runtimeBase && !runtimeBase.pathname.endsWith("/")) runtimeBase.pathname += "/";
    const payload = { artifacts, config: options.config, executionProvider: options.executionProvider ?? "wasm", allowCpuFallback: options.allowCpuFallback ?? false, runtimeBaseUrl: runtimeBase?.href };
    await this.request("init", payload, transfers, options.signal);
  }

  /** Copies the exact caller view so transfer cannot detach the caller's PDF buffer. */
  async parse(pdf: Uint8Array, options: ParseOptions = {}): Promise<DocumentResult> {
    this.assertReady(options.signal);
    const bytes = new Uint8Array(pdf);
    return await this.request("parse", { bytes }, [bytes.buffer], options.signal) as DocumentResult;
  }

  /** Reuses Rust renderers without running PDF extraction or model inference again. */
  async render(document: DocumentResult, format: "json" | "text" | "markdown"): Promise<string> {
    this.assertReady();
    return await this.request("render", { document, format }) as string;
  }

  /** Terminates the Worker and settles in-flight work; repeated close is harmless. */
  async close(): Promise<void> { if (this.state !== "closed") this.fail(new DocParseError("ParserClosed", "Parser was closed"), "closed"); }

  /** Rejects concurrent or terminal requests before allocating transfer buffers. */
  private assertReady(signal?: AbortSignal): void {
    if (this.state === "closed" || this.state === "failed") throw new DocParseError("ParserClosed", "Create a new parser before reusing a closed instance");
    if (this.state !== "ready") throw new DocParseError("ParserBusy", "Parser already has an operation in progress");
    if (signal?.aborted) throw new DocParseError("Aborted", "Request was already aborted");
  }

  /** Associates one response and its AbortSignal with exactly one request lifetime. */
  private request(method: string, payload: unknown, transfers: Transferable[] = [], signal?: AbortSignal): Promise<unknown> {
    if (signal?.aborted) return Promise.reject(new DocParseError("Aborted", "Request was already aborted"));
    if (this.pending) return Promise.reject(new DocParseError("ParserBusy", "Parser already has an operation in progress"));
    const initializing = method === "init";
    const id = ++this.nextId;
    this.state = initializing ? "initializing" : "busy";
    return new Promise((resolve, reject) => {
      const abort = () => this.fail(new DocParseError("Aborted", "Operation aborted; create a new parser to continue"), "closed");
      const cleanup = () => signal?.removeEventListener("abort", abort);
      this.pending = { id, resolve, reject, cleanup, initializing };
      signal?.addEventListener("abort", abort, { once: true });
      try { this.worker.postMessage({ id, method, payload }, transfers); }
      catch (error) { this.fail(new DocParseError("WorkerStopped", String(error)), "failed"); }
    });
  }

  /** Makes terminal transitions once and prevents late messages from settling a later request. */
  private fail(error: Error, state: "failed" | "closed"): void {
    this.state = state;
    this.worker.terminate();
    const pending = this.pending;
    this.pending = undefined;
    if (pending) { pending.cleanup(); pending.reject(error); }
  }
}

/** Resolves only after the real model and shared parser are initialized inside the Worker. */
export async function createParser(options: WebParserOptions): Promise<DocParser> {
  if (options.signal?.aborted) throw new DocParseError("Aborted", "Initialization was already aborted");
  const parser = new WorkerParser();
  try { await parser.initialize(options); return parser; }
  catch (error) { await parser.close(); throw error; }
}
