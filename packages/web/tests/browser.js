const parameters = new URL(location.href).searchParams;
const sdkUrl = parameters.has("relocated") ? new URL("/relocated-sdk/index.js", location.href) : new URL("../dist/index.js", import.meta.url);
const { createParser } = await import(sdkUrl.href);

const output = document.querySelector("#results");
const report = { runId: `${parameters.get("run") ?? "browser"}-${crypto.randomUUID().slice(0, 8)}`, startedAt: Date.now(), sequence: 0, browser: navigator.userAgent, sdk: sdkUrl.href, tests: [], metrics: [], status: "running" };
const start = performance.now();
const NativeWorker = Worker;

/** Substitutes only a recording entry script; it imports the actual production Worker. */
globalThis.Worker = class extends NativeWorker {
  constructor(url, options) {
    const instrumented = new URL("./instrumented-worker.js", import.meta.url);
    instrumented.searchParams.set("worker", String(url));
    if (parameters.has("noGpu")) instrumented.searchParams.set("noGpu", "1");
    if (parameters.has("failGpuInit")) instrumented.searchParams.set("failGpuInit", "1");
    if (parameters.has("growMemory")) instrumented.searchParams.set("growMemory", "1");
    if (parameters.has("legacyMemory")) instrumented.searchParams.set("legacyMemory", "1");
    super(instrumented, options);
    this.addEventListener("message", event => {
      if (event.data.metrics) { report.metrics.push(event.data.metrics); render(); }
    });
  }
};

/** Publishes progress so the test is inspectable without developer-console execution. */
function render() {
  report.sequence++;
  output.textContent = JSON.stringify(report, null, 2);
  // Persist evidence even when an embedding app reclaims a background browser tab.
  fetch("/__docparse_test_report", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(report), keepalive: report.status !== "running" }).then(response => response.arrayBuffer()).catch(() => {});
}
/** Fails immediately when an independently specified runtime contract is violated. */
function assert(value, message) { if (!value) throw new Error(message); }
/** Requires the exact public error category and catches unexpected success. */
async function rejects(operation, code) {
  try { await operation; } catch (error) { assert(error.code === code, `Expected ${code}, got ${error.code}: ${error.message}`); return; }
  throw new Error(`Expected rejection ${code}`);
}
/** Compares all canonical fields while retaining strict strings and bounded numeric differences. */
function differences(actual, expected, path = "", found = []) {
  if (typeof actual === "number" && typeof expected === "number") {
    if (Math.abs(actual - expected) > 1e-3) found.push({ path, actual, expected });
  } else if (actual && expected && typeof actual === "object" && typeof expected === "object") {
    const keys = new Set([...Object.keys(actual), ...Object.keys(expected)]);
    for (const key of keys) differences(actual[key], expected[key], `${path}.${key}`, found);
  } else if (actual !== expected) {
    // These diagnostics contain a numeric model weight inside otherwise exact text.
    const numericDiagnostic = /\.diagnostics\.order\.removed_edge\.\d+$/.test(path);
    const pattern = /^(.* weight=)([+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?)( reason=.*)$/;
    const left = numericDiagnostic && typeof actual === "string" ? actual.match(pattern) : null;
    const right = numericDiagnostic && typeof expected === "string" ? expected.match(pattern) : null;
    if (left && right && left[1] === right[1] && left[3] === right[3] && Math.abs(Number(left[2]) - Number(right[2])) <= 1e-3) {
      (report.diagnosticNumericDifferences ??= []).push({path, actual, expected});
    } else found.push({ path, actual, expected });
  }
  return found;
}

/** Refuses to compare results derived from different PDF bytes. */
async function verifyReferenceInput(bytes, name) {
  const response = await fetch(new URL(`../test-results/native/${name}.sha256`, import.meta.url));
  assert(response.ok, `Missing reference input checksum for ${name}`);
  const hash = [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map(byte => byte.toString(16).padStart(2, "0")).join("");
  assert(hash === (await response.text()).trim(), `PDF changed after generating the native reference: ${name}`);
}

const base = new URL("../../../", import.meta.url);
const options = { artifacts: { kind: "urls", model: new URL("models/pp-doclayout-v3/inference.onnx", base).href, config: new URL("models/pp-doclayout-v3/inference.yml", base).href, manifest: new URL("models/pp-doclayout-v3/model-manifest.json", base).href } };
options.executionProvider = parameters.get("provider") === "webgpu" ? "webgpu" : "wasm";
if (parameters.has("fallback")) options.allowCpuFallback = true;
let parser;
let parityFailures = 0;
const gpuUnavailable = parameters.has("noGpu") || parameters.has("failGpuInit");
if (gpuUnavailable && options.executionProvider === "webgpu" && !options.allowCpuFallback) {
  try {
    await rejects(createParser(options), parameters.has("noGpu") ? "ExecutionProviderUnavailable" : "ExecutionProviderInitializationFailed");
    report.tests.push("PASS: unavailable WebGPU fails explicitly without CPU fallback");
    report.status = "passed";
  } catch (error) { report.status = "failed"; report.error = error.stack ?? String(error); }
  report.elapsedMs = Math.round(performance.now() - start); render();
} else try {
  const initialization = new AbortController();
  parser = await createParser({ ...options, signal: initialization.signal });
  report.executionProvider = parser.executionProvider;
  assert(parser.executionProvider === (gpuUnavailable ? "wasm" : options.executionProvider), "Reported backend differs from the initialized backend");
  initialization.abort();
  report.tests.push("PASS: actual module Worker initialization and detached initialization signal"); render();
  const bytes = new Uint8Array(await (await fetch(new URL("crates/core/tests/fixtures/pdf/extraction_metadata.pdf", base))).arrayBuffer());
  const storage = new Uint8Array(bytes.length + 16); storage.set(bytes, 8);
  const parsing = parser.parse(storage.subarray(8, 8 + bytes.length));
  await rejects(parser.parse(bytes), "ParserBusy");
  const document = await parsing;
  assert(!document.pages.some(page => page.warnings.some(warning => warning.code === "LayoutUnavailable")), "Real-model acceptance must not silently use layout fallback");
  if (parameters.has("growMemory")) assert(report.metrics.at(-1).forcedMemoryGrowth > 0, "Parser heap growth was not exercised");
  const inferenceMetrics = report.metrics.at(-1);
  const expectViews = parameters.has("expectViews") || (!parameters.has("legacyMemory") && typeof WebAssembly.Memory.prototype.toResizableBuffer === "function");
  if (expectViews) {
    assert(inferenceMetrics.resizableMemory, "Parser memory is not resizable");
    assert(inferenceMetrics.memoryConversion.before === inferenceMetrics.memoryConversion.after, "Enabling stable views changed the allocated memory size");
    assert(inferenceMetrics.memoryConversion.maximum === 4294967296, "Parser memory does not declare the wasm32 ceiling");
    assert(inferenceMetrics.borrowedInputBytes > 0 && inferenceMetrics.copiedInputBytes === 0, "Inference inputs still require intermediate copies");
    report.tests.push("PASS: growing parser memory retains borrowed inputs with zero intermediate copied bytes");
  }
  if (parameters.has("legacyMemory")) {
    assert(!inferenceMetrics.resizableMemory && inferenceMetrics.copiedInputBytes > 0 && inferenceMetrics.borrowedInputBytes === 0, "Legacy memory did not use safe input snapshots");
    report.tests.push("PASS: legacy memory retains safe inputs across heap growth");
  }
  const actualProviders = inferenceMetrics.providers.map(provider => typeof provider === "string" ? provider : provider.name);
  assert(actualProviders.includes(parser.executionProvider), "ORT did not receive the selected backend");
  if (parser.executionProvider === "webgpu") assert(inferenceMetrics.gpuSubmissions > 0, "WebGPU parse submitted no GPU commands");
  else assert(inferenceMetrics.gpuSubmissions === 0, "CPU parse unexpectedly submitted GPU commands");
  report.tests.push(`PASS: actual ${parser.executionProvider} backend and GPU command submission contract`);
  assert(storage.byteLength === bytes.length + 16, "Caller buffer was detached");
  assert(document.pages.length === 1 && document.errors.length === 0, "Single-page parsing degraded or failed");
  assert((await parser.render(document, "text")).length > 0, "Rust text renderer returned no content");
  report.tests.push("PASS: real PDF bytes, offset view, Busy, canonical result and Rust renderer"); render();

  await verifyReferenceInput(bytes, "extraction_metadata");
  const referenceResponse = await fetch(new URL("../test-results/native/extraction_metadata.json", import.meta.url));
  assert(referenceResponse.ok, "Generate native reference fixtures before browser acceptance");
  const nativeSingle = await referenceResponse.json();
  const textFacts = value => value.pages.map(page => page.blocks.flatMap(block => block.lines.flatMap(line => line.text_items.map(item => item.raw_text))).join(""));
  assert(JSON.stringify(textFacts(document)) === JSON.stringify(textFacts(nativeSingle)), "Unembedded-font single page lost or reordered raw text");
  const delta = differences(document, nativeSingle);
  report.parityDifferenceCount = delta.length;
  report.parityDifferences = delta.slice(0, 12);
  if (delta.length) { report.tests.push(`RECORDED: unembedded-font single-page platform differences (${delta.length} fields)`); }
  else report.tests.push("PASS: native/Web single-page parity (all fields, numeric tolerance 1e-3)");
  render();

  const alreadyAborted = new AbortController(); alreadyAborted.abort();
  await rejects(parser.parse(bytes, { signal: alreadyAborted.signal }), "Aborted");
  await rejects(parser.parse(new Uint8Array([1, 2, 3])), "DocumentFailed");
  const cycles = Number(new URL(location.href).searchParams.get("cycles") ?? 3);
  for (let index = 0; index < cycles; index++) {
    const repeated = await parser.parse(bytes);
    assert(differences(repeated, document).length === 0, "Repeated parsing changed canonical output");
    const metrics = report.metrics.at(-1);
    assert(metrics.liveTensors === 0, `Runtime tensors retained: ${metrics.liveTensors}`);
    assert(metrics.sessions === 1, `Unexpected live session count: ${metrics.sessions}`);
    assert(JSON.stringify(metrics.fetches.sort()) === '["fetch_name_0","fetch_name_1"]', "Rust fetches were not forwarded to the runtime");
    assert(JSON.stringify(metrics.outputs.sort()) === '["fetch_name_0","fetch_name_1"]', "Unneeded mask returned by runtime");
    report.completedCycles = index + 1; render();
  }
  report.tests.push(`PASS: ${cycles} repeat parses, bad-PDF recovery, zero retained tensors, exactly two runtime fetches`); render();

  const multiple = new Uint8Array(await (await fetch(new URL("crates/core/tests/fixtures/pdf/multipage_layout.pdf", base))).arrayBuffer());
  const progressEvents = [], pageImages = [];
  let settled = false, lateEvent = false;
  const multiResult = await parser.parse(multiple, {
    onProgress: event => { lateEvent ||= settled; progressEvents.push(event); },
    onPageImage: image => { lateEvent ||= settled; pageImages.push(image); },
  });
  settled = true;
  assert(multiResult.pages.length === 3 && multiResult.errors.length === 0, "Three-page parsing degraded or failed");
  assert(progressEvents[0].stage === "opening" && progressEvents.at(-1).stage === "complete", "Progress boundaries are missing");
  for (const stage of ["scanning", "analyzing"]) {
    assert(JSON.stringify(progressEvents.filter(event => event.stage === stage).map(event => event.completed)) === "[0,1,2,3]", `${stage} progress is not based on actual page completion`);
  }
  assert(JSON.stringify(pageImages.map(image => image.pageNumber).sort()) === "[1,2,3]", "Missing or duplicate PDFium page images");
  for (const image of pageImages) {
    assert(image.blob instanceof Blob && image.blob.type === "image/png", "Page image is not a PNG Blob");
    const bitmap = await createImageBitmap(image.blob);
    assert(bitmap.width === image.width && bitmap.height === image.height, "PNG dimensions differ from the PDFium raster");
    bitmap.close();
  }
  assert(!lateEvent, "Observer events arrived after settlement");
  report.tests.push("PASS: real page progress, PNG image delivery before settlement, and valid image dimensions"); render();
  await verifyReferenceInput(multiple, "multipage_layout");
  const multiReference = await (await fetch(new URL("../test-results/native/multipage_layout.json", import.meta.url))).json();
  assert(JSON.stringify(textFacts(multiResult)) === JSON.stringify(textFacts(multiReference)), "Unembedded-font multipage parsing lost or reordered raw text");
  const multiDelta = differences(multiResult, multiReference);
  report.multipageDifferenceCount = multiDelta.length;
  report.multipageDifferences = multiDelta.slice(0, 12);
  if (multiDelta.length) { report.tests.push(`RECORDED: unembedded-font multipage platform differences (${multiDelta.length} fields)`); }
  else report.tests.push("PASS: actual three-page parsing and native/Web parity");
  render();

  const embedded = new Uint8Array(await (await fetch(new URL("crates/core/tests/fixtures/pdf/embedded_layout.pdf", base))).arrayBuffer());
  const embeddedResult = await parser.parse(embedded);
  assert(embeddedResult.pages.length === 2 && embeddedResult.errors.length === 0, "Embedded-font parsing degraded");
  await verifyReferenceInput(embedded, "embedded_layout");
  const embeddedReference = await (await fetch(new URL("../test-results/native/embedded_layout.json", import.meta.url))).json();
  const embeddedDelta = differences(embeddedResult, embeddedReference);
  report.embeddedDifferenceCount = embeddedDelta.length;
  report.embeddedDifferences = embeddedDelta.slice(0, 12);
  if (embeddedDelta.length) { parityFailures++; report.tests.push(`FAIL: embedded-font strict parity (${embeddedDelta.length} fields)`); }
  else report.tests.push("PASS: two-page embedded-font strict native/Web parity");
  render();

  const cjk = new Uint8Array(await (await fetch(new URL("crates/core/tests/fixtures/pdf/embedded_cjk_90.pdf", base))).arrayBuffer());
  const cjkResult = await parser.parse(cjk);
  assert(cjkResult.pages.length === 1 && cjkResult.errors.length === 0, "Chinese geometry fixture degraded");
  await verifyReferenceInput(cjk, "embedded_cjk_90");
  const cjkReference = await (await fetch(new URL("../test-results/native/embedded_cjk_90.json", import.meta.url))).json();
  const cjkDelta = differences(cjkResult, cjkReference);
  report.cjkDifferenceCount = cjkDelta.length;
  report.cjkDifferences = cjkDelta.slice(0, 12);
  report.cjkGeometry = cjkResult.pages.map(page => ({width:page.width,height:page.height,rotation:page.rotation,diagnostics:page.diagnostics,warnings:page.warnings}));
  report.cjkRaw = textFacts(cjkResult);
  report.cjkNativeRaw = textFacts(cjkReference);
  render();
  // Rotated glyphs may render as separate lines; the original raw facts must stay intact.
  assert(textFacts(cjkResult).join("").includes("中文文档解析测试"), "Chinese raw text was not preserved");
  assert(cjkResult.pages[0].width === 1504 && cjkResult.pages[0].height === 1144, "CropBox/UserUnit geometry was not applied");
  assert(cjkResult.pages[0].rotation === 90, "Page rotation was lost");
  if (cjkDelta.length) { parityFailures++; report.tests.push(`FAIL: Chinese/rotation/CropBox/UserUnit parity (${cjkDelta.length} fields)`); }
  else report.tests.push("PASS: embedded Chinese, rotation, CropBox and UserUnit strict parity");
  render();

  const cancellation = new AbortController();
  const cancelled = rejects(parser.parse(multiple, { signal: cancellation.signal }), "Aborted");
  await new Promise(resolve => setTimeout(resolve, 100)); cancellation.abort();
  await cancelled;
  await rejects(parser.parse(bytes), "ParserClosed");
  await parser.close(); await parser.close();
  report.tests.push("PASS: active cancellation, terminal parser state, idempotent close"); render();

  await rejects(createParser({ ...options, config: { layout: { model_path: "/fake.onnx" } } }), "InvalidConfig");
  const replacement = await createParser(options);
  try { assert((await replacement.parse(bytes)).pages.length === 1, "New Worker failed after cancellation"); }
  finally { await replacement.close(); }
  report.tests.push("PASS: filesystem configuration rejection and explicit recreation after cancellation");
  if (expectViews) assert(report.metrics.every(metrics => metrics.copiedInputBytes === 0), "Lifecycle acceptance introduced intermediate input copies");
  if (parameters.has("legacyMemory")) assert(report.metrics.every(metrics => metrics.borrowedInputBytes === 0), "Legacy inference retained a detachable input view");
  report.status = parityFailures ? "failed" : "passed";
} catch (error) { report.status = "failed"; report.error = error.stack ?? String(error); }
finally { if (parser) await parser.close(); report.elapsedMs = Math.round(performance.now() - start); render(); }
