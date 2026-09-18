// Model-level browser integration: real ONNX weights, separate from WebUI acceptance.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, realpath } from "node:fs/promises";
import { dirname, resolve, extname, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const provider = process.argv[2] ?? "wasm";
assert(["wasm", "webgpu"].includes(provider));
const routes = { "/pkg/": "target/texo-browser", "/models/": "models/texo", "/fixtures/": "crates/formula-texo/tests/fixtures", "/ort/": "packages/wasm-web/dist/ort" };
const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, "http://localhost");
    response.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    response.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
    if (url.pathname === "/favicon.ico") { response.statusCode = 204; response.end(); return; }
    if (url.pathname === "/") { response.setHeader("Content-Type", "text/html"); response.end("<!doctype html><title>Texo model test</title>"); return; }
    if (url.pathname === "/worker.mjs") {
      response.setHeader("Content-Type", "text/javascript");
      response.end(`import init, {BrowserTexo} from '/pkg/browser.js';
        self.onmessage = async ({data: webgpu}) => { try {
          let gpuSubmissions = 0;
          if (webgpu) {
            const submit = GPUQueue.prototype.submit;
            GPUQueue.prototype.submit = function(commands) { gpuSubmissions++; return submit.call(this, commands); };
          }
          await init();
          const bytes = async p => new Uint8Array(await (await fetch(p)).arrayBuffer());
          const started = performance.now();
          const model = await BrowserTexo.load(...await Promise.all(['encoder_model.onnx','decoder_model_merged.onnx','tokenizer.json'].map(n => bytes('/models/'+n))), location.origin+'/ort/', webgpu);
          const loadMs = performance.now()-started;
          const locationViolations = [];
          const ortRun = globalThis.ort.InferenceSession.prototype.run;
          globalThis.ort.InferenceSession.prototype.run = async function(...args) {
            const outputs = await ortRun.apply(this, args);
            if (webgpu) {
              for (const [name, value] of Object.entries(outputs)) {
                if (name !== 'logits' && value.dims.every(n => n > 0) && value.location !== 'gpu-buffer') locationViolations.push(name+':'+value.location);
              }
              for (const [name, value] of Object.entries(args[0])) {
                if ((name === 'encoder_hidden_states' || name.startsWith('past_key_values')) && value.dims.every(n => n > 0) && value.location !== 'gpu-buffer') locationViolations.push('input '+name+':'+value.location);
              }
            }
            return outputs;
          };
          const png = await bytes('/fixtures/formula_single.png');
          const warmup = JSON.parse(await model.recognize(png, 1));
          const run = performance.now();
          const batch = JSON.parse(await model.recognize(png, 2));
          self.postMessage({loadMs, batchMs:performance.now()-run, warmup, batch, gpuSubmissions, locationViolations});
          model.free();
        } catch(e) { self.postMessage({error:String(e), stack:e?.stack}); } };`);
      return;
    }
    const route = Object.entries(routes).find(([prefix]) => url.pathname.startsWith(prefix));
    assert(route, "unknown route");
    const base = await realpath(resolve(root, route[1]));
    const path = await realpath(resolve(base, decodeURIComponent(url.pathname.slice(route[0].length))));
    assert(path.startsWith(base + sep), "path outside assets");
    response.setHeader("Content-Type", ({".js":"text/javascript", ".mjs":"text/javascript", ".wasm":"application/wasm", ".json":"application/json"})[extname(path)] ?? "application/octet-stream");
    response.end(await readFile(path));
  } catch (error) { response.statusCode = 500; response.end(String(error)); }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
// Headless Chrome may require explicit WebGPU enablement to expose its available adapter.
const browser = await chromium.launch({channel: "chrome", headless: true, args: provider === "webgpu" ? ["--enable-unsafe-webgpu"] : []});
try {
  const page = await browser.newPage();
  const shapeWarnings = [];
  // Validate the corrected export without hiding ORT's output-shape diagnostics.
  page.on("console", message => {
    const text = message.text();
    if (/VerifyOutputSizes|Expected shape from model.*does not match actual shape/s.test(text)) shapeWarnings.push(text);
    console.error(text.slice(0, 500));
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const result = await page.evaluate(webgpu => new Promise((resolve, reject) => {
    const worker = new Worker('/worker.mjs', {type:'module'});
    const timeout = setTimeout(() => { worker.terminate(); reject(new Error('Texo browser deadline')); }, 120000);
    worker.onmessage = ({data}) => { clearTimeout(timeout); worker.terminate(); resolve(data); };
    worker.onerror = error => { clearTimeout(timeout); worker.terminate(); reject(new Error(error.message)); };
    worker.postMessage(webgpu);
  }), provider === "webgpu");
  assert.equal(result.error, undefined, result.stack ?? result.error);
  const reference = JSON.parse(await readFile(resolve(root, routes["/fixtures/"], "reference.json"), "utf8"));
  const expected = reference.cases.find(c => c.image === "formula_single.png").latex;
  assert.deepEqual(result.locationViolations, [], "hidden states and nonempty caches must stay on GPU");
  assert.deepEqual(result.warmup, [expected]);
  assert.deepEqual(result.batch, [expected, expected]);
  assert.deepEqual(shapeWarnings, [], "Texo must not emit output-shape mismatch warnings");
  if (provider === "webgpu") assert(result.gpuSubmissions > 0, "WebGPU must submit actual GPU work");
  console.log(JSON.stringify({provider, browser: browser.version(), ...result}));
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
