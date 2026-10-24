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
const inputContract = process.argv[3] ?? "original";
assert(["original", "reordered", "missing", "duplicate", "unknown"].includes(inputContract));
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
        self.onmessage = async ({data: {webgpu, inputContract}}) => { try {
          let gpuSubmissions = 0;
          if (webgpu) {
            const submit = GPUQueue.prototype.submit;
            GPUQueue.prototype.submit = function(commands) { gpuSubmissions++; return submit.call(this, commands); };
          }
          await init();
          // Alter only reported decoder metadata; inference still uses the real model and runtime.
          if (inputContract !== 'original') Object.defineProperty(globalThis, 'ort', {
            configurable: true,
            /** Installs metadata fault injection when the production loader publishes ORT. */
            set(runtime) {
              Object.defineProperty(globalThis, 'ort', {value: runtime, writable: true, configurable: true});
              const create = runtime.InferenceSession.create;
              /** Preserves real session execution while exposing reordered or invalid input contracts. */
              runtime.InferenceSession.create = async function(...args) {
                const session = await create.apply(this, args);
                if (session.inputNames.includes('input_ids')) {
                  const metadata = [...session.inputMetadata];
                  if (inputContract === 'reordered') metadata.reverse();
                  if (inputContract === 'missing') metadata.pop();
                  if (inputContract === 'duplicate') metadata[1] = metadata[0];
                  if (inputContract === 'unknown') metadata[0] = {...metadata[0], name: 'unexpected_input'};
                  Object.defineProperties(session, {
                    inputNames: {value: metadata.map(input => input.name)},
                    inputMetadata: {value: metadata},
                  });
                }
                return session;
              };
            },
          });
          const bytes = async p => new Uint8Array(await (await fetch(p)).arrayBuffer());
          const started = performance.now();
          const model = await BrowserTexo.load(...await Promise.all(['encoder_model.onnx','decoder_model_merged.onnx','tokenizer.json'].map(n => bytes('/models/'+n))), location.origin+'/ort/', webgpu);
          const loadMs = performance.now()-started;
          // Invalid contracts must fail during loading, before a crop reaches the decoder.
          if (!['original', 'reordered'].includes(inputContract)) { model.free(); self.postMessage({loaded: true}); return; }
          // Inspect ORT's real device rather than inferring hardware execution from the requested provider.
          const info = webgpu ? (await globalThis.ort.env.webgpu.device).adapterInfo : null;
          const adapter = info ? {vendor:info.vendor,architecture:info.architecture,isFallbackAdapter:info.isFallbackAdapter} : null;
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
          self.postMessage({loadMs, batchMs:performance.now()-run, warmup, batch, gpuSubmissions, locationViolations, adapter});
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
// Use Linux's hardware GL/Vulkan path instead of validating only SwiftShader's software implementation.
const gpuArgs = process.platform === "linux"
  ? ["--enable-unsafe-webgpu", "--use-angle=gl", "--enable-features=Vulkan", "--ignore-gpu-blocklist"] : [];
const browser = await chromium.launch({channel: "chrome", headless: true, args: provider === "webgpu" ? gpuArgs : []});
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
  const result = await page.evaluate(options => new Promise((resolve, reject) => {
    const worker = new Worker('/worker.mjs', {type:'module'});
    const timeout = setTimeout(() => { worker.terminate(); reject(new Error('Texo browser deadline')); }, 120000);
    worker.onmessage = ({data}) => { clearTimeout(timeout); worker.terminate(); resolve(data); };
    worker.onerror = error => { clearTimeout(timeout); worker.terminate(); reject(new Error(error.message)); };
    worker.postMessage(options);
  }), {webgpu: provider === "webgpu", inputContract});
  const contractErrors = {missing: /missing required inputs/, duplicate: /duplicate Texo decoder input/, unknown: /unexpected Texo decoder input/};
  if (inputContract in contractErrors) {
    assert.match(result.error ?? "", contractErrors[inputContract], "Invalid decoder contract must fail during initialization");
    console.log(JSON.stringify({provider, inputContract, rejected: result.error}));
  } else {
    assert.equal(result.error, undefined, result.stack ?? result.error);
    const reference = JSON.parse(await readFile(resolve(root, routes["/fixtures/"], "reference.json"), "utf8"));
    const expected = reference.cases.find(c => c.image === "formula_single.png").latex;
    assert.deepEqual(result.locationViolations, [], "hidden states and nonempty caches must stay on GPU");
    assert.deepEqual(result.warmup, [expected]);
    assert.deepEqual(result.batch, [expected, expected]);
    assert.deepEqual(shapeWarnings, [], "Texo must not emit output-shape mismatch warnings");
    if (provider === "webgpu") assert(result.gpuSubmissions > 0, "WebGPU must submit actual GPU work");
    console.log(JSON.stringify({provider, inputContract, browser: browser.version(), ...result}));
  }
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
