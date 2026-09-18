import init, { default_config, WebParser } from "./pkg/docparse_web.js";
import type { DocumentResult, FormulaSource, ModelSource, ParserProgress, ParserTiming, WebParseConfig, TsrTableInput } from "./types.js";
import type { WorkerInbound, WorkerResponse, WorkerSuccess, TsrCropPixels } from "./protocol.js";
import { artifact } from "./artifacts.js";
import { formulaEngineError } from "./configuration.js";

const scope = globalThis as unknown as DedicatedWorkerGlobalScope;
let parser: WebParser | undefined;
let executing = false;

/** Makes uncaught WASM and Promise failures terminal instead of leaving callers suspended. */
function fatal(reason: unknown): void {
  scope.postMessage({ fatal: true, code: "WorkerStopped", message: reason instanceof Error ? reason.message : String(reason), stack: reason instanceof Error ? reason.stack : undefined } satisfies WorkerResponse);
  scope.close();
}
scope.addEventListener("error", (event) => { event.preventDefault(); fatal(event.error || event.message); });
scope.addEventListener("unhandledrejection", (event) => { event.preventDefault(); fatal(event.reason); });

/** Encodes the inference raster in the Worker so only compressed page blobs reach the UI. */
async function pageImage(pageNumber: number, width: number, height: number, pixels: Uint8Array) {
  const canvas = new OffscreenCanvas(width, height);
  const context = canvas.getContext("2d");
  if (!context) throw new Error("A 2D canvas is required for page previews");
  const image = context.createImageData(width, height);
  for (let source = 0, target = 0; source < pixels.length; source += 3, target += 4) {
    image.data[target] = pixels[source]; image.data[target + 1] = pixels[source + 1];
    image.data[target + 2] = pixels[source + 2]; image.data[target + 3] = 255;
  }
  context.putImageData(image, 0, 0);
  return { pageNumber, width, height, blob: await canvas.convertToBlob({ type: "image/png" }) };
}

/** Rejects filesystem settings and merges only business values into Rust-provided defaults. */
function configuration(overrides: WebParseConfig | undefined): unknown {
  const raw = default_config() as Record<string, Record<string, unknown>>;
  for (const [group, values] of Object.entries(overrides ?? {})) {
    if (!Object.hasOwn(raw, group) || !values || typeof values !== "object" || Array.isArray(values)) throw Object.assign(new Error(`Unknown or invalid configuration group ${group}`), { code: "InvalidConfig" });
    for (const key of Object.keys(values)) {
      // OCR's nested file groups are native-only; browser models arrive through the artifact API.
      if (["layout", "tsr", "ocr", "formula"].includes(group) && ["model_path", "tokenizer_path", "model_config_path", "model_manifest_path", "execution_provider", "detection", "recognition", "orientation"].includes(key)) throw Object.assign(new Error(`Web configuration does not accept ${group}.${key}`), { code: "InvalidConfig" });
    }
    const merged = { ...raw[group], ...values };
    if (group === "formula" && Object.hasOwn(values, "engine")) {
      const engine = (values as Record<string, unknown>).engine;
      const formulaEnabled = merged.inline_enabled !== false || merged.display_enabled !== false;
      const invalidEngine = formulaEngineError(engine, formulaEnabled);
      if (engine === undefined || invalidEngine) throw Object.assign(new Error(invalidEngine ?? "formula.engine must be an object"), { code: "InvalidConfig" });
      // Replace the tagged object; paths belonging to the other default variant must not leak across.
      // Omit inactive service settings, including NaN from a cleared input, before Rust deserialization.
      merged.engine = !formulaEnabled && (engine as { type: string }).type === "mineru" ? { type: "mineru" } : engine;
    }
    if (group === "tsr" && Object.hasOwn(values, "cell_detection")) {
      const cells = (values as Record<string, unknown>).cell_detection;
      // Batch size is a model limit validated by Rust; filesystem paths remain native-only.
      if (!cells || typeof cells !== "object" || Array.isArray(cells) || Object.keys(cells).some(key => !["enabled", "model", "score_threshold", "batch_size"].includes(key))) throw Object.assign(new Error("Web cell detection accepts enabled, model, score_threshold and batch_size only"), { code: "InvalidConfig" });
      // Preserve nested defaults; native paths remain unused with verified Worker artifacts.
      merged.cell_detection = { ...(raw[group].cell_detection as Record<string, unknown>), ...cells };
    }
    raw[group] = merged;
  }
  return raw;
}

/** Resolves each model family through the same verified artifact cache and progress channel. */
async function modelBytes(source: ModelSource, prefix: string, progress?: (value: ParserProgress) => void) {
  const [model, config, manifest] = source.kind === "urls"
    ? await Promise.all([artifact(`${prefix}_model`, source.model, progress), artifact(`${prefix}_config`, source.config, progress), artifact(`${prefix}_manifest`, source.manifest, progress)])
    : [source.model, source.config, source.manifest];
  return { model, config, manifest };
}

/** Loads the selected formula's distinct assets without treating decoder weights as a manifest. */
async function formulaBytes(source: FormulaSource, progress?: (value: ParserProgress) => void) {
  if (source.type === "texo") {
    const encoder = source.kind === "urls" ? await artifact("formula_encoder", source.encoder, progress) : source.encoder;
    const decoder = source.kind === "urls" ? await artifact("formula_decoder", source.decoder, progress) : source.decoder;
    const tokenizer = source.kind === "urls" ? await artifact("formula_tokenizer", source.tokenizer, progress) : source.tokenizer;
    return { texo_formula: { encoder, decoder, tokenizer } };
  }
  const model = source.kind === "urls" ? await artifact("formula_model", source.model, progress) : source.model;
  const config = source.kind === "urls" ? await artifact("formula_tokenizer", source.tokenizer, progress) : source.tokenizer;
  const manifest = source.kind === "urls" ? await artifact("formula_manifest", source.manifest, progress) : source.manifest;
  return { formula: { model, config, manifest } };
}

/** Pending table promises are correlated separately from the enclosing parse operation. */
const tableRequests = new Map<string, { parseId: number; resolve: (value: TsrTableInput) => void; reject: (error: Error) => void }>();

/** Releases a single in-flight provider and asks the calling thread to abort its application work. */
function cancelTable(parseId: number, requestId: string): void {
  const pending = tableRequests.get(requestId);
  if (!pending || pending.parseId !== parseId) return;
  tableRequests.delete(requestId);
  pending.reject(new Error("External table request canceled"));
  scope.postMessage({ id: parseId, event: "table_structure_cancel", requestId } satisfies WorkerResponse);
}

/** Encodes an owned crop before exposing it to the caller, preserving the exact pixel coordinate contract. */
function requestTable(parseId: number, crop: TsrCropPixels): Promise<TsrTableInput> {
  return new Promise((resolve, reject) => {
    if (tableRequests.has(crop.request_id)) { reject(new Error("Duplicate table request ID")); return; }
    tableRequests.set(crop.request_id, { parseId, resolve, reject });
    void pageImage(crop.page_number, crop.width, crop.height, crop.pixels).then(({ blob }) => {
      if (!tableRequests.has(crop.request_id)) return;
      const { pixels: _pixels, width, height, ...metadata } = crop;
      scope.postMessage({ id: parseId, event: "table_structure_request", value: { ...metadata, image: { width, height, blob } } } satisfies WorkerResponse);
    }).catch(error => {
      const pending = tableRequests.get(crop.request_id);
      if (pending?.parseId === parseId) { tableRequests.delete(crop.request_id); pending.reject(error instanceof Error ? error : new Error(String(error))); }
    });
  });
}

scope.addEventListener("message", async (event: MessageEvent<WorkerInbound>) => {
  // Responses must be processed while Rust awaits the external provider; they are not new operations.
  if (event.data.method === "table_structure_reply") {
    const reply = event.data;
    const pending = tableRequests.get(reply.requestId);
    if (!pending || pending.parseId !== reply.id) return;
    tableRequests.delete(reply.requestId);
    if (reply.ok) pending.resolve(reply.value); else pending.reject(new Error(reply.message));
    return;
  }
  const { id, method, payload } = event.data;
  if (executing) { scope.postMessage({ id, ok: false, code: "ParserBusy", message: "Worker is busy" } satisfies WorkerResponse); return; }
  executing = true;
  const requestStarted = performance.now();
  const progress = payload.observeProgress
    ? (value: ParserProgress) => scope.postMessage({ id, event: "progress", value } satisfies WorkerResponse)
    : undefined;
  const timing = payload.observeTiming
    ? (value: ParserTiming) => scope.postMessage({ id, event: "timing", value } satisfies WorkerResponse)
    : undefined;
  try {
    let reply: WorkerSuccess;
    if (method === "init") {
      if (parser) throw new Error("Parser already initialized");
      progress?.({ stage: "loading_runtime" });
      const runtimeStarted = performance.now();
      try { await init(); }
      finally { timing?.({ stage: "runtime_load", page_number: null, duration_ms: performance.now() - runtimeStarted }); }
      const config = configuration(payload.config);
      if (!["wasm", "webgpu"].includes(payload.executionProvider)) throw Object.assign(new Error("Unknown execution provider"), { code: "InvalidConfig" });
      let webgpu = payload.executionProvider === "webgpu";
      if (webgpu) {
        try {
          const gpu = (navigator as unknown as { gpu?: { requestAdapter(): Promise<unknown> } }).gpu;
          if (!gpu || !await gpu.requestAdapter()) throw new Error("WebGPU is not available in this Worker");
        } catch (error) {
          // Adapter rejection and a missing adapter have the same capability semantics.
          if (!payload.allowCpuFallback) throw Object.assign(new Error(`WebGPU unavailable: ${String(error)}`), { code: "ExecutionProviderUnavailable" });
          console.warn("WebGPU unavailable; using explicitly allowed CPU fallback", error);
          webgpu = false;
        }
      }
      const source = payload.artifacts;
      const downloadStarted = performance.now();
      const [model, modelConfig, manifest] = source.kind === "urls"
        ? await Promise.all([artifact("model", source.model, progress), artifact("config", source.config, progress), artifact("manifest", source.manifest, progress)])
        : [source.model, source.config, source.manifest];
      if (source.kind === "urls") timing?.({ stage: "model_download", page_number: null, duration_ms: performance.now() - downloadStarted });
      const tableArtifacts = payload.tsrArtifacts ? await modelBytes(payload.tsrArtifacts, "tsr", progress) : undefined;
      const tableCellArtifacts = payload.tsrCellArtifacts ? await modelBytes(payload.tsrCellArtifacts, "tsr_cell_detection", progress) : undefined;
      const ocrSource = payload.ocrArtifacts;
      // Load sequentially to bound transient copies of large recognition weights in a Worker.
      const ocrArtifacts = ocrSource ? {
        detection: await modelBytes(ocrSource.detection, "ocr_detection", progress),
        recognition: await modelBytes(ocrSource.recognition, "ocr_recognition", progress),
        orientation: ocrSource.orientation ? await modelBytes(ocrSource.orientation, "ocr_orientation", progress) : undefined,
      } : undefined;
      const formulaArtifacts = payload.formulaArtifacts ? await formulaBytes(payload.formulaArtifacts, progress) : {};
      const auxiliaryArtifacts = { tsr: tableArtifacts, tsr_cell_detection: tableCellArtifacts, ocr: ocrArtifacts, ...formulaArtifacts };
      const base = payload.runtimeBaseUrl ?? new URL("./ort/", import.meta.url).href;
      progress?.({ stage: "initializing_model" });
      const modelStarted = performance.now();
      try { parser = await WebParser.create(model, modelConfig, manifest, config, base, webgpu, auxiliaryArtifacts); }
      catch (error) {
        const code = (error as { code?: string }).code;
        if (!webgpu || !payload.allowCpuFallback || !["ExecutionProviderUnavailable", "ExecutionProviderInitializationFailed"].includes(code ?? "")) throw error;
        console.warn("WebGPU initialization failed; using explicitly allowed CPU fallback");
        parser = await WebParser.create(model, modelConfig, manifest, config, base, false, auxiliaryArtifacts);
        webgpu = false;
      }
      finally { timing?.({ stage: "model_init", page_number: null, duration_ms: performance.now() - modelStarted }); }
      // Report the initialized backend, not the caller's preference, so fallback stays visible.
      reply = { id, ok: true, method, value: webgpu ? "webgpu" : "wasm" };
    } else if (!parser) throw new Error("Parser is not initialized");
    else if (method === "parse") {
      const images: Promise<void>[] = [];
      let imageFailure: unknown;
      let completion: ParserProgress | undefined;
      const onProgress = progress ? (event: ParserProgress) => {
        if (event.stage === "complete") completion = event;
        else progress(event);
      } : undefined;
      const onImage = payload.pageImages ? (page: number, width: number, height: number, pixels: Uint8Array) => {
        // Catch immediately: a PNG failure must not become an unhandled rejection while
        // Rust is awaiting inference. The parse promise settles only after all images do.
        const encodingStarted = performance.now();
        images.push(pageImage(page, width, height, pixels)
          .then(value => scope.postMessage({ id, event: "page_image", value } satisfies WorkerResponse))
          .catch(error => { imageFailure ??= error; })
          .finally(() => timing?.({ stage: "preview_encode", page_number: page, duration_ms: performance.now() - encodingStarted })));
      } : undefined;
      // Rust validates the canonical result before its WASM ABI serializes it.
      let document: DocumentResult;
      try { document = await parser.parse_with_options(payload.bytes, payload.table, {
        progress: onProgress, page_image: onImage, timing,
        table_request: payload.externalTables ? (crop: TsrCropPixels) => requestTable(id, crop) : undefined,
        table_cancel: payload.externalTables ? (requestId: string) => cancelTable(id, requestId) : undefined,
      }); }
      finally { await Promise.all(images); }
      if (imageFailure) throw Object.assign(new Error(`Page preview failed: ${String(imageFailure)}`), { code: "ImageEncodingFailed" });
      if (completion) progress?.(completion);
      reply = { id, ok: true, method, value: document };
    }
    else if (method === "render") reply = { id, ok: true, method, value: parser.render(payload.document, payload.format) };
    else throw new Error("Unknown Worker method");
    // Includes preview encoding and result serialization, but excludes result message delivery.
    timing?.({ stage: "worker_total", page_number: null, duration_ms: performance.now() - requestStarted });
    scope.postMessage(reply);
  } catch (error) {
    timing?.({ stage: "worker_total", page_number: null, duration_ms: performance.now() - requestStarted });
    const failure = error as { code?: string; message?: string };
    scope.postMessage({ id, ok: false, code: failure?.code ?? "OperationFailed", message: failure?.message ?? String(error), stack: error instanceof Error ? error.stack : undefined } satisfies WorkerResponse);
  } finally { for (const [requestId, pending] of tableRequests) if (pending.parseId === id) cancelTable(id, requestId); executing = false; }
});
