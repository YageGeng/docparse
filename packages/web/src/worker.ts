import init, { default_config, WebParser } from "./pkg/docparse_web.js";
import type { DocumentResult, ParserProgress, ParserTiming, WebParseConfig } from "./types.js";
import type { WorkerRequest, WorkerResponse, WorkerSuccess } from "./protocol.js";
import { artifact } from "./artifacts.js";

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
      if (group === "layout" && ["model_path", "model_config_path", "model_manifest_path", "execution_provider"].includes(key)) throw Object.assign(new Error(`Web configuration does not accept layout.${key}`), { code: "InvalidConfig" });
    }
    raw[group] = { ...raw[group], ...values };
  }
  return raw;
}

scope.addEventListener("message", async (event: MessageEvent<WorkerRequest>) => {
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
      const base = payload.runtimeBaseUrl ?? new URL("./ort/", import.meta.url).href;
      progress?.({ stage: "initializing_model" });
      const modelStarted = performance.now();
      try { parser = await WebParser.create(model, modelConfig, manifest, config, base, webgpu); }
      catch (error) {
        const code = (error as { code?: string }).code;
        if (!webgpu || !payload.allowCpuFallback || !["ExecutionProviderUnavailable", "ExecutionProviderInitializationFailed"].includes(code ?? "")) throw error;
        console.warn("WebGPU initialization failed; using explicitly allowed CPU fallback");
        parser = await WebParser.create(model, modelConfig, manifest, config, base, false);
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
      try { document = await parser.parse_with_observer(payload.bytes, onProgress, onImage, timing); }
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
  } finally { executing = false; }
});
