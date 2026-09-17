// Real SDK + example integration: selectable formula models, presets, and owned byte transfers.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "../node_modules/playwright/index.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const pdfPath = resolve(process.argv[2] ?? "target/texo-sdk-formulas.pdf");
const pdf = Array.from(await readFile(pdfPath));
const output = resolve(root, "packages/wasm-web/test-results/formula-engines");
await mkdir(output, { recursive: true });
const server = spawn(process.execPath, [resolve(root, "packages/wasm-web/scripts/serve-example.mjs")], { env: { ...process.env, PORT: "0" }, stdio: ["ignore", "pipe", "inherit"] });
let browser;
try {
  const url = await new Promise((resolve, reject) => {
    let text = "";
    server.stdout.on("data", chunk => { text += chunk; const match = text.match(/http:\/\/127\.0\.0\.1:\d+\/example\//); if (match) resolve(match[0]); });
    server.on("error", reject);
    server.on("exit", code => reject(new Error(`Example server exited: ${code}`)));
  });
  browser = await chromium.launch({ channel: "chrome", headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  page.on("pageerror", error => console.error(error));
  await page.addInitScript(() => {
    window.formulaFlow = { documents: [], terminated: 0, init: [] };
    const NativeWorker = Worker;
    window.Worker = class extends NativeWorker {
      /** Observe the actual parser without replacing model calls or results. */
      constructor(url, options) {
        super(url, options);
        this.addEventListener("message", ({ data }) => {
          if (data.ok && data.method === "parse") window.formulaFlow.documents.push(data.value);
        });
      }
      /** Record selected engine metadata without retaining large artifact buffers. */
      postMessage(message, transfers) {
        if (message.method === "init") window.formulaFlow.init.push({ engine: message.payload.config.formula.engine.type, source: message.payload.formulaArtifacts?.type });
        super.postMessage(message, transfers);
      }
      /** Count model-setting teardown before the next model is prepared. */
      terminate() { window.formulaFlow.terminated++; super.terminate(); }
    };
  });
  await page.goto(url);
  const results = [];
  for (const [engine, provider, mode] of (process.argv.includes("--ui-only") ? [] : [["texo", "wasm", "preset"], ["pp", "wasm", "preset"], ["texo", "webgpu", "preset"], ["pp", "webgpu", "legacy"], ["texo", "webgpu", "bytes"]])) {
    const result = await page.evaluate(async ({ engine, provider, mode, pdf }) => {
      const { createParser } = await import("/dist/index.js");
      const options = { artifacts: { kind: "urls", model: "/models/inference.onnx", config: "/models/inference.yml", manifest: "/models/model-manifest.json" }, executionProvider: provider, config: { tsr: { mode: "rules_only" }, ocr: { policy: "disabled" }, formula: { engine: { type: engine }, batch_size: 2 } } };
      let owned;
      if (mode === "legacy") {
        delete options.config.formula.engine;
        options.formulaArtifacts = { kind: "urls", model: "/models/pp-formulanet-plus-s/inference.onnx", tokenizer: "/models/pp-formulanet-plus-s/tokenizer.json", manifest: "/models/pp-formulanet-plus-s/model-manifest.json" };
      } else if (mode === "bytes") {
        delete options.config.formula.engine;
        const load = async name => new Uint8Array(await (await fetch(`/models/texo/${name}`)).arrayBuffer());
        owned = await Promise.all(["encoder_model.onnx", "decoder_model_merged.onnx", "tokenizer.json"].map(load));
        options.formulaArtifacts = { type: "texo", kind: "bytes", encoder: owned[0], decoder: owned[1], tokenizer: owned[2] };
      }
      let parser;
      try {
        parser = await createParser(options);
        const document = await parser.parse(new Uint8Array(pdf));
        const markdown = await parser.render(document, "markdown");
        return { engine, provider: parser.executionProvider, mode, errors: document.errors, formulas: document.pages.flatMap(p => p.formulas ?? []), markdown, bytesRetained: owned?.every(bytes => bytes.byteLength > 0) ?? true };
      } finally { await parser?.close(); }
    }, { engine, provider, mode, pdf });
    assert.equal(result.provider, provider);
    assert.deepEqual(result.errors, []);
    assert(result.formulas.length > 0, "Input PDF must contain detected formulas");
    assert(result.formulas.every(f => f.engine.startsWith(engine === "texo" ? "texo-transfer-onnx-" : "pp-formulanet-plus-s-onnx-") && f.latex && !f.error), JSON.stringify(result.formulas));
    assert(result.formulas.every(f => result.markdown.includes(f.latex)));
    assert(result.bytesRetained, "Worker transfer must not detach caller-owned model bytes");
    results.push(result);
    console.log(JSON.stringify({ engine, provider, mode, formulas: result.formulas.length }));
  }
  const invalid = await page.evaluate(async () => {
    const { createParser } = await import("/dist/index.js");
    const base = { artifacts: { kind: "urls", model: "/unused", config: "/unused", manifest: "/unused" }, config: { tsr: { mode: "rules_only" }, formula: { engine: { type: "texo" } } } };
    const cases = [
      { ...base, formulaArtifacts: { type: "pp", kind: "urls", model: "/unused", tokenizer: "/unused", manifest: "/unused" } },
      { ...base, formulaArtifacts: { type: "texo", kind: "urls", encoder: "/unused", tokenizer: "/unused" } },
    ];
    const errors = [];
    for (const options of cases) {
      try { const parser = await createParser(options); await parser.close(); errors.push("unexpected success"); }
      catch (error) { errors.push(error.code); }
    }
    return errors;
  });
  assert.deepEqual(invalid, ["InvalidConfig", "InvalidModelArtifacts"]);

  // The real example selects presets through one dropdown; users never enter formula paths.
  await page.reload();
  assert.equal(await page.locator("#formula-engine").inputValue(), "texo");
  await page.locator("#table-mode").selectOption("rules_only");
  await page.locator("#ocr-policy").selectOption("disabled");
  await page.locator("#file").setInputFiles(pdfPath);
  for (const engine of ["texo", "pp"]) {
    if (engine === "pp") {
      await page.locator("#formula-engine").selectOption("pp");
      assert.equal(await page.evaluate(() => window.formulaFlow.terminated), 1);
      assert(await page.locator("#prepare").isEnabled());
    }
    await page.locator("#parse").click();
    await page.waitForFunction(() => document.querySelector("#stage").textContent === "Your document is ready" || document.querySelector(".status-card").dataset.state === "error", undefined, { timeout: 120000 }).catch(async error => {
      const state = await page.evaluate(() => ({stage:document.querySelector("#stage").textContent, detail:document.querySelector("#status-detail").textContent, init:window.formulaFlow.init, documents:window.formulaFlow.documents.length}));
      await page.screenshot({path: `${output}/failure.png`});
      throw new Error(`${error.message}: ${JSON.stringify(state)}`);
    });
    assert.equal(await page.locator("#stage").textContent(), "Your document is ready", await page.locator("#status-detail").textContent());
    const formulas = await page.evaluate(() => window.formulaFlow.documents.at(-1).pages.flatMap(p => p.formulas ?? []));
    assert(formulas.length && formulas.every(f => f.engine.startsWith(engine === "texo" ? "texo-transfer-onnx-" : "pp-formulanet-plus-s-onnx-")));
  }
  assert.deepEqual(await page.evaluate(() => window.formulaFlow.init), [{ engine: "texo", source: "texo" }, { engine: "pp", source: "pp" }]);
  await page.screenshot({ path: resolve(output, "selector.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  assert(await page.locator("#formula-engine").isVisible());
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.screenshot({ path: resolve(output, "selector-narrow.png") });
  await writeFile(resolve(output, "results.json"), JSON.stringify({ browser: browser.version(), results, invalid, uiSwitch: "passed" }, null, 2));
  console.log("Formula engine presets, custom bytes, validation, and UI switching passed");
} finally {
  await browser?.close();
  server.kill("SIGTERM");
}
