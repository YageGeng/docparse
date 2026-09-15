import { createParser } from "/sdk/index.js";

const parameters = new URL(location.href).searchParams;
const layout = parameters.get("layout");
const run = parameters.get("run");
if (!["NCHW", "NHWC"].includes(layout)) throw new Error("Invalid preferredLayout");
const NativeWorker = Worker;
let metrics, parser;
const inputs = new Map();
const status = document.querySelector("#status");
const progress = { stage: "loading", file: null };

/** Imports the production release Worker through the existing session instrumentation. */
globalThis.Worker = class extends NativeWorker {
  constructor(url, options) {
    const entry = new URL("/tests/instrumented-worker.js", location.href);
    entry.searchParams.set("worker", String(url));
    entry.searchParams.set("preferredLayout", layout);
    entry.searchParams.set("benchmark", "1");
    super(entry, options);
    this.addEventListener("message", event => { if (event.data.metrics) metrics = event.data.metrics; });
  }
};

/** Hashes bytes without retaining an additional document-sized JavaScript object graph. */
async function sha256(bytes) {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map(value => value.toString(16).padStart(2, "0")).join("");
}

/** Loads verified PDF bytes and creates actual WebGPU sessions before any measured parse. */
async function initialize() {
  const manifest = await (await fetch("/manifest.json")).json();
  const inputStarted = performance.now();
  for (const input of manifest.inputs) {
    const response = await fetch(input.url);
    if (!response.ok) throw new Error(`PDF download failed: ${input.file}`);
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.length !== input.sizeBytes || await sha256(bytes) !== input.sha256) throw new Error(`PDF changed: ${input.file}`);
    inputs.set(input.index, bytes);
  }
  const inputLoadMs = performance.now() - inputStarted;
  const artifacts = name => ({ kind: "urls", model: `/models/${name}/inference.onnx`, config: `/models/${name}/inference.yml`, manifest: `/models/${name}/model-manifest.json` });
  const timings = [];
  const started = performance.now();
  parser = await createParser({
    artifacts: artifacts("pp-doclayout-v3"), tsrArtifacts: artifacts("slanet-plus"), tsrCellArtifacts: artifacts("rtdetr-table-cell-wireless"),
    executionProvider: "webgpu", allowCpuFallback: false, config: manifest.config,
    onTiming: timing => timings.push(timing),
    onProgress: event => { progress.stage = event.stage; status.textContent = `${layout}: ${event.stage}`; },
  });
  if (parser.executionProvider !== "webgpu" || metrics.sessions !== 3) throw new Error("Expected three real WebGPU sessions");
  for (const model of metrics.models) {
    if (!model.providers.some(provider => provider.name === "webgpu" && provider.preferredLayout === layout)) throw new Error("preferredLayout did not reach ORT");
  }
  return { inputLoadMs, initializationMs: performance.now() - started, timings, metrics };
}

/** Measures only the public parse promise; hashing and result persistence are measured separately. */
async function parseFile(input, measured) {
  const stages = {};
  let expectedPages;
  progress.file = input.file;
  const submissions = metrics.gpuSubmissions;
  const started = performance.now();
  const document = await parser.parse(inputs.get(input.index), {
    onTiming: event => {
      const stage = stages[event.stage] ??= { count: 0, totalMs: 0, maxMs: 0 };
      stage.count++; stage.totalMs += event.duration_ms; stage.maxMs = Math.max(stage.maxMs, event.duration_ms);
    },
    onProgress: event => {
      progress.stage = event.stage;
      progress.completed = event.completed; progress.total = event.total;
      if (event.total !== undefined) expectedPages = event.total;
      if (event.completed === undefined || event.completed % 16 === 0) status.textContent = `${layout}: ${input.file} · ${event.stage} ${event.completed ?? ""}/${event.total ?? ""}`;
    },
  });
  const parseMs = performance.now() - started;
  const gpuSubmissions = metrics.gpuSubmissions - submissions;
  const warnings = {};
  for (const page of document.pages) for (const warning of page.warnings) warnings[warning.code] = (warnings[warning.code] ?? 0) + 1;
  const degraded = document.errors.length > 0 || Object.keys(warnings).some(code => ["LayoutUnavailable", "OcrUnavailable", "OcrFailed", "TableExternalFailed", "TableExternalTimeout"].includes(code));
  if (degraded || document.pages.length !== expectedPages || gpuSubmissions === 0) throw new Error(`Invalid parse ${input.file}: ${JSON.stringify({ pages: document.pages.length, expectedPages, warnings, errors: document.errors, gpuSubmissions })}`);
  if (!measured) return { file: input.file, pages: document.pages.length, parseMs, stages, metrics };

  const validationStarted = performance.now();
  // Only execution-local correlation numbers are normalized; all content and floating-point values remain intact.
  for (const page of document.pages) for (const block of page.blocks) for (const evidence of block.evidence ?? []) {
    if (evidence.kind === "external_table_structure" && Object.hasOwn(evidence.details, "request_id")) evidence.details.request_id = "run-local";
  }
  const pages = [];
  for (const page of document.pages) {
    pages.push({ page: page.page_number, blocks: page.blocks.length,
      canonicalHash: await sha256(new TextEncoder().encode(JSON.stringify(page))),
      textHash: await sha256(new TextEncoder().encode(JSON.stringify(page.blocks.map(block => [block.label, block.text])))),
      tables: page.blocks.filter(block => block.table).length });
  }
  const json = JSON.stringify(document);
  const canonicalHash = await sha256(new TextEncoder().encode(json));
  const response = await fetch(`/result?run=${encodeURIComponent(run)}&index=${input.index}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: json });
  if (!response.ok) throw new Error(`Result persistence failed: ${response.status}`);
  return { file: input.file, index: input.index, pages: document.pages.length, parseMs, validationMs: performance.now() - validationStarted,
    stages, warnings, pageErrors: document.errors.length, degraded, gpuSubmissions, canonicalHash, pageSignatures: pages,
    models: metrics.models, memoryBytes: metrics.memoryBytes, resizableMemory: metrics.resizableMemory };
}

globalThis.layoutBenchmark = { initialize, parseFile, progress, metrics: () => metrics, close: () => parser?.close() };
status.textContent = `${layout}: ready to initialize`;
