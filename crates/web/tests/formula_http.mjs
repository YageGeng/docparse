// Real browser acceptance against the production WASM parser and a running HTTP formula service.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

const pdf = resolve(process.argv[2]);
const serverUrl = process.argv[3] ?? "http://127.0.0.1:18000/v1";
const output = resolve("target/http-browser");
await mkdir(output, { recursive: true });
const server = spawn(process.execPath, ["packages/wasm-web/scripts/serve-example.mjs"], { env: { ...process.env, PORT: "0" }, stdio: ["ignore", "pipe", "inherit"] });
let browser;
try {
  const url = await new Promise((resolve, reject) => {
    server.stdout.on("data", chunk => { const match = String(chunk).match(/http:\/\/127\.0\.0\.1:\d+\/example\//); if (match) resolve(match[0]); });
    server.on("error", reject);
    server.on("exit", code => reject(new Error(`Example server exited: ${code}`)));
  });
  browser = await chromium.launch({ channel: "chrome", headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors = [], formulaDownloads = [], serviceRequests = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("request", request => { if (/\/models\/(texo|pp-formulanet)/.test(request.url())) formulaDownloads.push(request.url()); });
  page.context().on("request", request => { if (request.url().includes("/v1/")) serviceRequests.push(request.url()); });
  await page.addInitScript(() => {
    window.httpFlow = { init: [], documents: [], terminated: 0 };
    const NativeWorker = Worker;
    window.Worker = class extends NativeWorker {
      /** Observes real parse results without changing the Worker or service responses. */
      constructor(url, options) {
        super(url, options);
        this.addEventListener("message", ({ data }) => { if (data.ok && data.method === "parse") window.httpFlow.documents.push(data.value); });
      }
      /** Records only model selection and artifact presence before forwarding untouched. */
      postMessage(message, transfer) {
        if (message.method === "init") window.httpFlow.init.push({ engine: message.payload.config.formula.engine, artifacts: Boolean(message.payload.formulaArtifacts?.some(Boolean)) });
        super.postMessage(message, transfer);
      }
      /** Counts actual parser disposal when service settings change. */
      terminate() { window.httpFlow.terminated++; super.terminate(); }
    };
  });
  await page.goto(url);
  const invalid = await page.evaluate(async () => {
    const { createParser } = await import("/dist/index.js");
    const codes = [];
    for (const engine of [{ type: "http", server_url: "" }, { type: "http", server_url: "http://localhost:8000", worker_size: 0 }, { type: "http", server_url: "file:///tmp/model" }]) {
      try { const parser = await createParser({ artifacts: { kind: "urls", model: "/unused", config: "/unused", manifest: "/unused" }, config: { render: { workers: 1, queue_size: 2 }, layout: { queue_size: 1 }, tsr: { queue_size: 1, cell_detection: { queue_size: 1 } }, ocr: { detection: { queue_size: 1 }, recognition: { queue_size: 1 }, orientation: { queue_size: 1 } }, formula: { queue_size: 1, engine: [engine] } } }); await parser.close(); codes.push("accepted"); }
      catch (error) { codes.push(error.code); }
    }
    return codes;
  });
  assert.deepEqual(invalid, ["InvalidConfig", "InvalidConfig", "InvalidConfig"]);
  assert(await page.locator("#http-settings").isHidden());
  await page.locator("#formula-engine").selectOption("http");
  assert(await page.getByLabel("HTTP server URL", { exact: true }).isVisible());
  await page.locator("#http-server-url").fill(serverUrl);
  await page.locator("#http-workers").fill("2");
  await page.locator("#execution-provider").selectOption("wasm");
  await page.locator("#table-mode").selectOption("rules_only");
  await page.locator("#ocr-policy").selectOption("disabled");
  assert.match(await page.locator("#processing-location").textContent(), /sent to your HTTP formula server/);
  await page.locator("#prepare").click();
  assert(await page.locator("#http-server-url").isDisabled());
  await page.waitForFunction(() => /Models ready|Could not|errors/.test(document.querySelector("#stage").textContent), null, { timeout: 300000 });
  assert.equal(await page.locator("#stage").textContent(), "Models ready", await page.locator("#status-detail").textContent());
  await page.locator("#file").setInputFiles(pdf);
  await page.locator("#parse").click();
  await page.waitForFunction(() => /Your document is ready|Could not|errors/.test(document.querySelector("#stage").textContent), null, { timeout: 300000 });
  assert.equal(await page.locator("#stage").textContent(), "Your document is ready", await page.locator("#status-detail").textContent());
  const flow = await page.evaluate(() => window.httpFlow);
  assert.deepEqual(flow.init, [{ engine: [{ type: "http", server_url: serverUrl, worker_size: 2 }], artifacts: false }]);
  const document = flow.documents[0];
  assert.deepEqual(document.errors, []);
  const formulas = document.pages.flatMap(page => page.formulas ?? []);
  assert(formulas.length > 0, "The real layout model must find formula regions in the supplied PDF");
  assert(formulas.every(formula => formula.engine === "formula-http" && formula.latex && !formula.error), JSON.stringify(formulas));
  assert(serviceRequests.length >= formulas.length, "Observe actual formula HTTP requests before checking the disabled path");
  assert.deepEqual(formulaDownloads, []);
  await page.screenshot({ path: resolve(output, "http.png") });
  await page.locator("#http-server-url").fill(serverUrl.replace(/\/$/, "") + "/");
  assert.equal(await page.evaluate(() => window.httpFlow.terminated), flow.terminated + 1);
  assert(await page.locator("#prepare").isEnabled());
  await page.setViewportSize({ width: 390, height: 844 });
  assert(await page.getByLabel("HTTP server URL", { exact: true }).isVisible());
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
  await page.screenshot({ path: resolve(output, "http-mobile.png") });
  // Disabled recognition must not depend on invalid, now-inactive service controls.
  await page.locator("#http-server-url").fill("");
  await page.locator("#http-workers").fill("");
  await page.locator("#display-formula-enabled").uncheck();
  await page.locator("#inline-formula-enabled").uncheck();
  assert(await page.locator("#http-server-url").isDisabled());
  const requestsBeforeDisabled = serviceRequests.length;
  await page.locator("#prepare").click();
  await page.waitForFunction(() => /Models ready|Could not|errors/.test(document.querySelector("#stage").textContent), null, { timeout: 300000 });
  assert.equal(await page.locator("#stage").textContent(), "Models ready", await page.locator("#status-detail").textContent());
  await page.locator("#parse").click();
  await page.waitForFunction(() => /Your document is ready|Could not|errors/.test(document.querySelector("#stage").textContent), null, { timeout: 300000 });
  assert.equal(await page.locator("#stage").textContent(), "Your document is ready", await page.locator("#status-detail").textContent());
  const disabled = await page.evaluate(() => window.httpFlow.documents.at(-1));
  assert.deepEqual(disabled.errors, []);
  assert.equal(disabled.pages.flatMap(page => page.formulas ?? []).length, 0);
  assert.equal(serviceRequests.length, requestsBeforeDisabled);
  assert.deepEqual(formulaDownloads, []);
  await page.locator("#display-formula-enabled").check();
  assert(await page.locator("#http-server-url").isEnabled());
  assert.equal(await page.locator("#http-server-url").evaluate(input => input.checkValidity()), false);
  await page.locator("#formula-engine").selectOption("texo");
  assert(await page.locator("#http-settings").isHidden());
  assert.match(await page.locator("#privacy-label").textContent(), /Local/);
  assert.deepEqual(errors, []);
  const summary = { status: "passed", provider: "wasm", serverUrl, formulas: formulas.length, formulaDownloads, checks: ["SDK validation", "editable URL and worker_size", "real WASM and HTTP inference", "no local formula weights", "setting change disposal", "mobile layout", "disabled recognition ignores invalid service settings"] };
  await writeFile(resolve(output, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
  console.log(JSON.stringify(summary));
} finally {
  await browser?.close();
  server.kill();
}
