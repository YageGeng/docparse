import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { readFile, readdir, mkdir, realpath, stat, writeFile, appendFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, resolve, join, extname, sep } from "node:path";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { chromium } from "playwright";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(packageRoot, "../..");
const { values } = parseArgs({ options: { "pdf-dir": { type: "string" }, output: { type: "string" }, smoke: { type: "boolean" } } });
assert(values["pdf-dir"] && values.output, "Pass --pdf-dir and a fresh --output directory");
const pdfDir = await realpath(values["pdf-dir"]);
const output = resolve(values.output);
await mkdir(output, { recursive: true });
const recordsPath = join(output, "measurements.jsonl");
await writeFile(recordsPath, "", { flag: "wx" });
const build = JSON.parse(await readFile(join(packageRoot, "dist/build-manifest.json"), "utf8"));
assert.equal(build.rustTarget, "wasm32-unknown-unknown");
assert(build.optimization.flags.includes("-O4"), "Use the production release build with wasm-opt -O4");
const wasmHash = createHash("sha256").update(await readFile(join(packageRoot, "dist/pkg/docparse_web_bg.wasm"))).digest("hex");
assert.equal(wasmHash, build.hashes["pkg/docparse_web_bg.wasm"]);
const inputs = [];
for (const file of (await readdir(pdfDir)).filter(file => file.toLowerCase().endsWith(".pdf")).sort()) {
  const path = join(pdfDir, file); const bytes = await readFile(path);
  inputs.push({ index: inputs.length, file, sizeBytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex"), url: `/inputs/${inputs.length}.pdf` });
}
assert(inputs.length > 0, "No PDFs found");
const warmup = inputs.find(input => input.file === "2603.01919v2.pdf");
assert(warmup, "This corpus benchmark warms model families with 2603.01919v2.pdf");
const selected = values.smoke ? [inputs.reduce((a, b) => a.sizeBytes < b.sizeBytes ? a : b)] : inputs;
const config = { formula: { inline_enabled: false, display_enabled: false }, layout: { score_threshold: 0.5, session_pool_size: 1 }, tsr: { mode: "tsr_only", max_in_flight: 1, timeout_ms: 60000 },
  ocr: { policy: "disabled" }, runtime: { page_concurrency: 1, render_queue_capacity: 1, blocking_task_limit: 1, continue_on_page_error: true },
  render: { dpi: 144, max_long_edge_pixels: 2400 }, output: { include_evidence: true, include_diagnostics: false } };
const manifest = { inputs, config };
await writeFile(join(output, "inputs.json"), JSON.stringify(manifest, null, 2));
await writeFile(join(output, "build-manifest.json"), JSON.stringify(build, null, 2));
const mime = { ".js": "text/javascript", ".mjs": "text/javascript", ".wasm": "application/wasm", ".json": "application/json" };

/** Serves only approved PDFs and production assets; result writes stay inside this run's directory. */
const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, "http://localhost");
    if (request.method === "POST" && url.pathname === "/result") {
      const run = url.searchParams.get("run"), index = Number(url.searchParams.get("index"));
      assert(/^(NCHW|NHWC)-[12]$/.test(run) && Number.isInteger(index) && inputs[index], "Invalid result identity");
      const directory = join(output, "documents", run); await mkdir(directory, { recursive: true });
      await pipeline(request, createWriteStream(join(directory, `${index}.json`), { flags: "wx" }));
      response.writeHead(200); response.end("{}"); return;
    }
    if (request.method !== "GET") { response.writeHead(405); response.end(); return; }
    if (url.pathname === "/") {
      response.writeHead(200, { "Content-Type": "text/html" }); response.end('<!doctype html><meta charset="utf-8"><title>DocParse WebGPU A/B</title><pre id="status">Loading release SDK</pre><script type="module" src="/tests/preferred-layout-browser.js"></script>'); return;
    }
    if (url.pathname === "/manifest.json") { response.writeHead(200, { "Content-Type": "application/json" }); response.end(JSON.stringify(manifest)); return; }
    let path;
    const inputMatch = url.pathname.match(/^\/inputs\/(\d+)\.pdf$/);
    if (inputMatch && inputs[Number(inputMatch[1])]) path = join(pdfDir, inputs[Number(inputMatch[1])].file);
    else {
      const routes = [["/sdk/", join(packageRoot, "dist")], ["/tests/", join(packageRoot, "tests")], ["/models/", join(root, "models")]];
      const route = routes.find(([prefix]) => url.pathname.startsWith(prefix));
      if (!route) { response.writeHead(404); response.end(); return; }
      const directory = await realpath(route[1]); path = await realpath(resolve(directory, decodeURIComponent(url.pathname.slice(route[0].length))));
      assert(path.startsWith(directory + sep), "Path escaped asset directory");
    }
    const info = await stat(path); assert(info.isFile());
    response.writeHead(200, { "Content-Type": mime[extname(path)] ?? "application/octet-stream", "Content-Length": info.size, "Cache-Control": "no-store" });
    await pipeline(createReadStream(path), response);
  } catch (error) { if (!response.headersSent) response.writeHead(500); response.end(String(error)); }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
console.log(`Benchmark assets: ${origin}`);
const order = values.smoke ? ["NCHW", "NHWC"] : ["NCHW", "NHWC", "NHWC", "NCHW"];
const counts = { NCHW: 0, NHWC: 0 };
let browser, progressTimer, deadline;
const interrupt = () => { void browser?.close(); server.close(); };
process.once("SIGINT", interrupt); process.once("SIGTERM", interrupt);

/** Flushes each completed checkpoint so interruption never loses an entire corpus trial. */
async function record(row) { await appendFile(recordsPath, JSON.stringify({ ...row, timestamp: new Date().toISOString() }) + "\n"); }

try {
  for (const layout of order) {
    const run = `${layout}-${++counts[layout]}`;
    browser = await chromium.launch({ channel: "chrome", headless: false, args: ["--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows"] });
    const context = await browser.newContext(); const page = await context.newPage();
    const browserCdp = await browser.newBrowserCDPSession(); const hardware = await browserCdp.send("SystemInfo.getInfo");
    assert(!/swiftshader|llvmpipe/i.test(JSON.stringify(hardware.gpu.devices)), "Software renderer is not a valid hardware WebGPU benchmark");
    const errors = []; page.on("pageerror", error => errors.push(String(error)));
    await page.goto(`${origin}/?layout=${layout}&run=${run}`);
    await page.waitForFunction(() => Boolean(globalThis.layoutBenchmark), null, { timeout: 30000 });
    // Progress stays outside result artifacts, and a stalled case cannot retain its browser indefinitely.
    progressTimer = setInterval(async () => {
      try { console.log(`${run}: ${JSON.stringify(await page.evaluate(() => layoutBenchmark.progress))}`); } catch {}
    }, 15000);
    deadline = setTimeout(() => { console.error(`${run}: case timeout`); void browser?.close(); }, 20 * 60 * 1000);
    await record({ kind: "start", run, layout, browser: browser.version(), hardware: hardware.gpu, wasmHash, config });
    const initialization = await page.evaluate(() => layoutBenchmark.initialize());
    await record({ kind: "initialized", run, ...initialization });
    const warming = performance.now();
    for (let repeat = 0; repeat < 2; repeat++) {
      const warm = await page.evaluate(input => layoutBenchmark.parseFile(input, false), warmup);
      assert(warm.metrics.models.every(model => model.calls > 0 && model.gpuSubmissions > 0), "Warmup missed a model family");
    }
    await record({ kind: "warmed", run, warmupMs: performance.now() - warming });
    let parseMs = 0, validationMs = 0, pages = 0;
    const started = performance.now();
    for (const input of selected) {
      console.log(`${run}: parsing ${input.file}`);
      const result = await page.evaluate(input => layoutBenchmark.parseFile(input, true), input);
      await record({ kind: "document", run, layout, ...result });
      parseMs += result.parseMs; validationMs += result.validationMs; pages += result.pages;
      console.log(`${run}: ${result.pages} pages, parse ${(result.parseMs / 1000).toFixed(3)}s`);
    }
    assert.deepEqual(errors, []);
    await record({ kind: "summary", run, layout, documents: selected.length, pages, parseMs, validationMs, corpusWallMs: performance.now() - started, pagesPerSecond: pages / (parseMs / 1000), valid: true });
    clearInterval(progressTimer); clearTimeout(deadline); progressTimer = deadline = undefined;
    await page.evaluate(() => layoutBenchmark.close()); await browser.close(); browser = undefined;
  }
} catch (error) { await record({ kind: "failure", message: error.stack ?? String(error) }); throw error; }
finally { clearInterval(progressTimer); clearTimeout(deadline); process.removeListener("SIGINT", interrupt); process.removeListener("SIGTERM", interrupt); await browser?.close(); await new Promise(resolve => server.close(resolve)); }
