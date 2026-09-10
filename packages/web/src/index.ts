export type * from "./types.js";
import type { DocParser, DocumentResult, ExecutionProvider, ModelSource, ParseOptions, RenderFormat, WebParserOptions, TsrTableRequest, TsrTableInput } from "./types.js";
import type { WorkerCommand, WorkerMethod, WorkerOperations, WorkerRequest, WorkerResponse, WorkerResult, WorkerSuccess, WorkerTableReply } from "./protocol.js";

/** A request failure that preserves its stable machine-readable category. */
export class DocParseError extends Error {
  /** Creates a portable error without browser or Rust resource handles. */
  constructor(public readonly code: string, message: string) { super(message); this.name = "DocParseError"; }
}

type Pending = { id: number; method: WorkerMethod; resolve: (value: WorkerSuccess) => void; reject: (error: Error) => void; cleanup: () => void; initializing: boolean; callbacks: ParseOptions };

/** Owns exactly one Worker and one outstanding request at a time. */
class WorkerParser implements DocParser {
  private readonly worker: Worker;
  private pending?: Pending;
  private nextId = 0;
  private readonly tableControllers = new Map<string, AbortController>();
  private provider: ExecutionProvider = "wasm";
  private state: "initializing" | "ready" | "busy" | "failed" | "closed" = "initializing";

  /** Installs fatal handlers before any initialization request is dispatched. */
  constructor() {
    this.worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module", name: "docparse" });
    this.worker.onerror = (event) => { event.preventDefault(); this.fail(new DocParseError("WorkerStopped", event.message || "DocParse Worker stopped"), "failed"); };
    this.worker.onmessageerror = () => this.fail(new DocParseError("WorkerStopped", "Worker response could not be decoded"), "failed");
    this.worker.onmessage = (event: MessageEvent<WorkerResponse>) => {
      const message = event.data;
      if (!message) return;
      if ("fatal" in message) { const error = new DocParseError(message.code || "WorkerStopped", message.message || "Worker failed"); if (message.stack) error.stack += `\nWorker cause: ${message.stack}`; this.fail(error, "failed"); return; }
      const pending = this.pending;
      if (!pending || message.id !== pending.id) return;
      // Progress and images belong to this request but do not settle it or release Busy.
      // Ignore late messages after cancellation through the same request-ID check.
      if ("event" in message) {
        if (message.event === "table_structure_request") { this.resolveTable(pending, message.value); return; }
        if (message.event === "table_structure_cancel") { this.tableControllers.get(message.requestId)?.abort(); this.tableControllers.delete(message.requestId); return; }
        try {
          if (message.event === "progress") pending.callbacks.onProgress?.(message.value);
          else if (message.event === "timing") pending.callbacks.onTiming?.(message.value);
          else pending.callbacks.onPageImage?.(message.value);
        } catch (error) { console.error("DocParse observer callback failed", error); }
        return;
      }
      if (message.ok && message.method !== pending.method) {
        this.fail(new DocParseError("WorkerStopped", "Worker replied to a different operation"), "failed");
        return;
      }
      this.cancelTables();
      this.pending = undefined;
      pending.cleanup();
      if (message.ok) { this.state = "ready"; pending.resolve(message); }
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
    const payload: WorkerOperations["init"]["payload"] = { artifacts, config: options.config, executionProvider: options.executionProvider ?? "wasm", allowCpuFallback: options.allowCpuFallback ?? false, runtimeBaseUrl: runtimeBase?.href, observeProgress: Boolean(options.onProgress), observeTiming: Boolean(options.onTiming) };
    this.provider = await this.request({ method: "init", payload }, transfers, options.signal, { onProgress: options.onProgress, onTiming: options.onTiming });
  }

  /** Reports the backend selected by the Worker after successful model initialization. */
  get executionProvider(): ExecutionProvider { return this.provider; }

  /** Copies the exact caller view so transfer cannot detach the caller's PDF buffer. */
  async parse(pdf: Uint8Array, options: ParseOptions = {}): Promise<DocumentResult> {
    this.assertReady(options.signal);
    if (options.onTableStructure !== undefined && typeof options.onTableStructure !== "function") throw new DocParseError("InvalidTableOptions", "onTableStructure must be a function");
    if (options.table?.mode && options.table.mode !== "rules_only" && !options.onTableStructure) throw new DocParseError("InvalidTableOptions", "External table mode requires onTableStructure");
    const bytes = new Uint8Array(pdf);
    return await this.request({ method: "parse", payload: { bytes, table: options.table, externalTables: Boolean(options.onTableStructure), observeProgress: Boolean(options.onProgress), observeTiming: Boolean(options.onTiming), pageImages: Boolean(options.onPageImage) } }, [bytes.buffer], options.signal, options);
  }

  /** Reuses Rust renderers without running PDF extraction or model inference again. */
  async render(document: DocumentResult, format: RenderFormat): Promise<string> {
    this.assertReady();
    return await this.request({ method: "render", payload: { document, format } });
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
  private request<M extends WorkerMethod>(command: WorkerCommand & { method: M }, transfers: Transferable[] = [], signal?: AbortSignal, callbacks: ParseOptions = {}): Promise<WorkerResult<M>> {
    if (signal?.aborted) return Promise.reject(new DocParseError("Aborted", "Request was already aborted"));
    if (this.pending) return Promise.reject(new DocParseError("ParserBusy", "Parser already has an operation in progress"));
    const initializing = command.method === "init";
    const id = ++this.nextId;
    this.state = initializing ? "initializing" : "busy";
    return new Promise((resolve, reject) => {
      const abort = () => this.fail(new DocParseError("Aborted", "Operation aborted; create a new parser to continue"), "closed");
      const cleanup = () => signal?.removeEventListener("abort", abort);
      // The response ID and method are checked before resolving this typed boundary.
      this.pending = { id, method: command.method, resolve: message => resolve(message.value as WorkerResult<M>), reject, cleanup, initializing, callbacks };
      signal?.addEventListener("abort", abort, { once: true });
      try { this.worker.postMessage({ id, ...command } satisfies WorkerRequest, transfers); }
      catch (error) { this.fail(new DocParseError("WorkerStopped", String(error)), "failed"); }
    });
  }

  /** Delivers provider results only to their still-active parse and table request. */
  private resolveTable(pending: Pending, request: TsrTableRequest): void {
    if (this.tableControllers.has(request.request_id)) return;
    const controller = new AbortController();
    this.tableControllers.set(request.request_id, controller);
    const active = () => this.pending === pending && !controller.signal.aborted;
    const reply = (result: { ok: true; value: TsrTableInput } | { ok: false; message: string }) => {
      if (active()) this.worker.postMessage({ id: pending.id, method: "table_structure_reply", requestId: request.request_id, ...result } satisfies WorkerTableReply);
    };
    void Promise.resolve().then(() => {
      if (!active()) throw new DocParseError("Aborted", "Table request canceled");
      const handler = pending.callbacks.onTableStructure;
      if (!handler) throw new DocParseError("InvalidTableOptions", "External table provider is unavailable");
      return handler(request, controller.signal);
    }).then(value => reply({ ok: true, value })).catch(error => {
      try { reply({ ok: false, message: error instanceof Error ? error.message : String(error) }); }
      catch { this.fail(new DocParseError("WorkerStopped", "Table response could not be transferred"), "failed"); }
    }).finally(() => { if (this.tableControllers.get(request.request_id) === controller) this.tableControllers.delete(request.request_id); });
  }

  /** Aborts application-owned work when its parse completes, fails, or closes. */
  private cancelTables(): void {
    for (const controller of this.tableControllers.values()) controller.abort();
    this.tableControllers.clear();
  }

  /** Makes terminal transitions once and prevents late messages from settling a later request. */
  private fail(error: Error, state: "failed" | "closed"): void {
    this.state = state;
    this.cancelTables();
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
