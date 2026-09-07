import init, { default_config, WebParser } from "./pkg/docparse_web.js";
import type { ModelSource, WebParseConfig } from "./index.js";

const scope = globalThis as unknown as DedicatedWorkerGlobalScope;
let parser: WebParser | undefined;
let executing = false;

/** Makes uncaught WASM and Promise failures terminal instead of leaving callers suspended. */
function fatal(reason: unknown): void {
  scope.postMessage({ fatal: true, code: "WorkerStopped", message: reason instanceof Error ? reason.message : String(reason), stack: reason instanceof Error ? reason.stack : undefined });
  scope.close();
}
scope.addEventListener("error", (event) => { event.preventDefault(); fatal(event.error || event.message); });
scope.addEventListener("unhandledrejection", (event) => { event.preventDefault(); fatal(event.reason); });

/** Fetches one named artifact without logging its potentially credential-bearing URL. */
async function artifact(name: string, url: string): Promise<Uint8Array> {
  const response = await fetch(url);
  if (!response.ok) throw Object.assign(new Error(`${name} download failed with HTTP ${response.status}`), { code: "ArtifactFetch" });
  return new Uint8Array(await response.arrayBuffer());
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

scope.addEventListener("message", async (event) => {
  const { id, method, payload } = event.data;
  if (executing) { scope.postMessage({ id, ok: false, code: "ParserBusy", message: "Worker is busy" }); return; }
  executing = true;
  try {
    let value: unknown;
    if (method === "init") {
      if (parser) throw new Error("Parser already initialized");
      await init();
      const config = configuration(payload.config);
      if (!["wasm", "webgpu"].includes(payload.executionProvider)) throw Object.assign(new Error("Unknown execution provider"), { code: "InvalidConfig" });
      let webgpu = payload.executionProvider === "webgpu";
      if (webgpu) {
        const gpu = (navigator as unknown as { gpu?: { requestAdapter(): Promise<unknown> } }).gpu;
        if (!gpu || !await gpu.requestAdapter()) {
          if (!payload.allowCpuFallback) throw Object.assign(new Error("WebGPU is not available in this Worker"), { code: "ExecutionProviderUnavailable" });
          console.info("WebGPU unavailable; using explicitly allowed CPU fallback");
          webgpu = false;
        }
      }
      const source = payload.artifacts as ModelSource;
      const [model, modelConfig, manifest] = source.kind === "urls"
        ? await Promise.all([artifact("model", source.model), artifact("config", source.config), artifact("manifest", source.manifest)])
        : [source.model, source.config, source.manifest];
      const base = payload.runtimeBaseUrl ?? new URL("./ort/", import.meta.url).href;
      try { parser = await WebParser.create(model, modelConfig, manifest, config, base, webgpu); }
      catch (error) {
        const code = (error as { code?: string }).code;
        if (!webgpu || !payload.allowCpuFallback || !["ExecutionProviderUnavailable", "ExecutionProviderInitializationFailed"].includes(code ?? "")) throw error;
        console.warn("WebGPU initialization failed; using explicitly allowed CPU fallback");
        parser = await WebParser.create(model, modelConfig, manifest, config, base, false);
      }
      value = undefined;
    } else if (!parser) throw new Error("Parser is not initialized");
    else if (method === "parse") value = await parser.parse(payload.bytes);
    else if (method === "render") value = parser.render(payload.document, payload.format);
    else throw new Error("Unknown Worker method");
    scope.postMessage({ id, ok: true, value });
  } catch (error) {
    const failure = error as { code?: string; message?: string };
    scope.postMessage({ id, ok: false, code: failure?.code ?? "OperationFailed", message: failure?.message ?? String(error), stack: error instanceof Error ? error.stack : undefined });
  } finally { executing = false; }
});
