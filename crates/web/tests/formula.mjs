import assert from "node:assert/strict";
import { createReadStream } from "node:fs";
import { readFile, writeFile, mkdir, realpath, stat } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, resolve, extname, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const pdf = await realpath(process.argv[2]);
const output = resolve(process.argv[3] ?? "target/formula-browser.json");
const provider = process.argv[4] ?? "webgpu";
assert(["webgpu", "wasm"].includes(provider));
const recognition = process.argv[5] ?? "all";
assert(["all", "inline", "display", "off", "softmax"].includes(recognition));
const build = JSON.parse(await readFile(resolve(root, "packages/wasm-web/dist/build-manifest.json"), "utf8"));
assert(build.optimization.flags.includes("-O4"), "Run the production release build first");
const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, "http://localhost");
    if (url.pathname === "/") { response.writeHead(200, { "Content-Type": "text/html" }); response.end("<!doctype html><title>Formula integration</title><p>Real formula recognition</p>"); return; }
    let path;
    if (url.pathname === "/input.pdf") path = pdf;
    else {
      const route = [["/sdk/", "packages/wasm-web/dist"], ["/models/", "models"]].find(([prefix]) => url.pathname.startsWith(prefix));
      assert(route, "Unknown asset path");
      const directory = await realpath(resolve(root, route[1]));
      path = await realpath(resolve(directory, decodeURIComponent(url.pathname.slice(route[0].length))));
      assert(path.startsWith(directory + sep), "Asset escaped its directory");
    }
    const size = (await stat(path)).size;
    response.writeHead(200, { "Content-Length": size, "Content-Type": ({ ".js": "text/javascript", ".mjs": "text/javascript", ".wasm": "application/wasm", ".json": "application/json" })[extname(path)] ?? "application/octet-stream" });
    await pipeline(createReadStream(path), response);
  } catch (error) { if (!response.headersSent) response.writeHead(500); response.end(String(error)); }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const browser = await chromium.launch({ channel: "chrome", headless: true });
const page = await browser.newPage();
const recorder = await readFile(resolve(root, "packages/wasm-web/tests/instrumented-worker.js"), "utf8");
await page.route("**/__formula-observe.js?*", route => route.fulfill({ contentType: "text/javascript", body: recorder }));
await page.addInitScript(benchmarkGpu => {
  const NativeWorker = Worker;
  window.Worker = class extends NativeWorker {
    /** Observes actual sessions without changing the selected provider or model outputs. */
    constructor(url, options) {
      const entry = new URL("/__formula-observe.js", location.href);
      entry.searchParams.set("worker", String(url));
      if (benchmarkGpu) entry.searchParams.set("benchmark", "1");
      super(entry, options);
      this.addEventListener("message", ({ data }) => { if (data.metrics) window.formulaMetrics = data.metrics; });
    }
  };
}, provider === "webgpu");
page.on("console", message => console.log(message.text().slice(0, 400)));
const deadline = setTimeout(() => { console.error("Formula browser deadline exceeded"); void browser.close(); }, 600000);
try {
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const result = await page.evaluate(async ({ provider, recognition }) => {
    const { createParser } = await import("/sdk/index.js");
    let parser;
    try {
      const started = performance.now();
      parser = await createParser({ executionProvider: provider, allowCpuFallback: false,
        artifacts: { kind: "urls", model: "/models/pp-doclayout-v3/inference.onnx", config: "/models/pp-doclayout-v3/inference.yml", manifest: "/models/pp-doclayout-v3/model-manifest.json" },
        formulaArtifacts: recognition === "off" ? undefined : { kind: "urls", model: "/models/pp-formulanet-plus-s/inference.onnx", tokenizer: "/models/pp-formulanet-plus-s/tokenizer.json", manifest: "/models/pp-formulanet-plus-s/model-manifest.json" },
        config: { tsr: { mode: "rules_only" }, formula: { inline_enabled: !["display", "off"].includes(recognition), display_enabled: !["inline", "off"].includes(recognition), batch_size: 2, timeout_ms: 120000 } },
        onProgress: progress => { if (progress.stage !== "downloading") console.log(JSON.stringify(progress)); },
      });
      const initializationMs = performance.now() - started;
      const bytes = new Uint8Array(await (await fetch("/input.pdf")).arrayBuffer());
      const parsing = performance.now();
      const document = await parser.parse(bytes);
      const parseMs = performance.now() - parsing;
      const markdown = await parser.render(document, "markdown");
      return { provider: parser.executionProvider, initializationMs, parseMs, pages: document.pages.length, errors: document.errors,
        nativeText: document.pages.map(page => page.blocks.map(block => ({ id: block.id, text: block.text }))),
        formulas: document.pages.flatMap(page => page.formulas ?? []), markdown, metrics: window.formulaMetrics };
    } catch (error) { return { error: error.code, message: error.message, metrics: window.formulaMetrics }; }
    finally { await parser?.close(); }
  }, { provider, recognition });
  await mkdir(dirname(output), { recursive: true });
  await writeFile(output, JSON.stringify({ ...result, browser: browser.version(), wasmHash: build.hashes["pkg/docparse_web_bg.wasm"] }, null, 2));
  assert.equal(result.error, undefined, result.message);
  assert.equal(result.provider, provider);
  assert.equal(result.errors.length, 0);
  assert.equal(result.formulas.length === 0, recognition === "off");
  if (recognition === "inline") assert(result.formulas.every(formula => formula.label === "inline_formula"));
  if (recognition === "display") assert(result.formulas.every(formula => formula.label === "display_formula"));
  if (process.argv[5] === "softmax") {
    const formula = result.formulas.find(formula => formula.id.startsWith("p4:") && formula.bbox.left < 300 && formula.bbox.top > 670 && formula.latex?.replaceAll(" ", "").includes("softmax"));
    assert(formula && !formula.error, "Missing page-four softmax normalization formula");
    assert((formula.latex.replace(/\s/g, "").match(/_\{i\}/g) ?? []).length >= 2, "Both sum and component indices must survive");
    assert(formula.crop_bbox?.right >= 281.49, "Recognition crop omitted the final native subscript");
    assert(formula.text_spans.some(span => span.bbox.right > formula.bbox.right), "Recovered script needs an exact source span");
    assert(result.markdown.includes(formula.latex));
    console.log(JSON.stringify({ checkedFormula: formula }));
  } else {
    assert(result.formulas.every(formula => formula.latex && formula.markdown && !formula.error), "Formula inference must complete rather than degrade");
    assert(result.formulas.every(formula => result.markdown.includes(formula.latex)), "Markdown omitted a recognized formula");
  }
  if (pdf.includes("Terminal-Universe_")) assert(result.markdown.includes("learning rate of"), "Formula replacement erased adjacent source prose");
  assert.equal(result.metrics.sessions, recognition === "off" ? 1 : 2);
  if (provider === "webgpu") assert(result.metrics.models.every(model => model.calls > 0 && model.gpuSubmissions > 0));
  console.log(JSON.stringify({ pages: result.pages, formulas: result.formulas.length, parseMs: result.parseMs, provider, recognition }));
} finally { clearTimeout(deadline); await browser.close(); await new Promise(resolve => server.close(resolve)); }
