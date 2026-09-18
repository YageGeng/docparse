// Exercise the production SDK and real layout model with multiple page deliveries and cancellation.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

const root = new URL("../../../", import.meta.url);
const bytes = [...await readFile(new URL("crates/core/tests/fixtures/pdf/multipage_layout.pdf", root))];
const server = spawn(process.execPath, [new URL("../../../packages/wasm-web/scripts/serve-example.mjs", import.meta.url).pathname], {
  env: { ...process.env, PORT: "0" }, stdio: ["ignore", "pipe", "inherit"],
});
let browser;
try {
  const origin = await new Promise((resolve, reject) => {
    let output = "";
    server.stdout.on("data", chunk => {
      output += chunk;
      const match = output.match(/http:\/\/127\.0\.0\.1:\d+/);
      if (match) resolve(match[0]);
    });
    server.once("error", reject);
    server.once("exit", code => reject(new Error(`Static artifact server exited: ${code}`)));
  });
  browser = await chromium.launch({ channel: "chrome", headless: true });
  const page = await browser.newPage();
  await page.goto(`${origin}/example/`);
  const report = await page.evaluate(async bytes => {
    const { createParser } = await import("/dist/index.js");
    const options = {
      artifacts: { kind: "urls", model: "/models/inference.onnx", config: "/models/inference.yml", manifest: "/models/model-manifest.json" },
      executionProvider: "wasm",
      config: {
        render: { workers: 1, queue_size: 2, max_long_edge_pixels: 800 },
        layout: { session_size: 1, queue_size: 2 },
        tsr: { queue_size: 1, mode: "rules_only", cell_detection: { queue_size: 1 } },
        ocr: { policy: "disabled", detection: { queue_size: 1 }, recognition: { queue_size: 1 }, orientation: { queue_size: 1 } },
        formula: { queue_size: 1, inline_enabled: false, display_enabled: false },
      },
    };
    const runs = [];
    for (const capacity of [1, 2]) {
      options.config.render.queue_size = capacity;
      const parser = await createParser(options);
      try {
        const document = await parser.parse(new Uint8Array(bytes));
        runs.push({ capacity, pages: document.pages.map(page => page.page_number), errors: document.errors, text: await parser.render(document, "text") });
      } finally { await parser.close(); }
    }
    let parser = await createParser(options);
    let canceled;
    try {
      const controller = new AbortController();
      await parser.parse(new Uint8Array(bytes), {
        signal: controller.signal,
        onProgress: event => { if (event.stage === "analyzing") controller.abort(); },
      });
    } catch (error) { canceled = error.code ?? error.name; }
    finally { await parser.close(); }
    parser = await createParser(options);
    try {
      const restored = await parser.parse(new Uint8Array(bytes));
      return { runs, canceled, restoredPages: restored.pages.length, restoredErrors: restored.errors };
    } finally { await parser.close(); }
  }, bytes);
  for (const run of report.runs) {
    assert.deepEqual(run.pages, [1, 2, 3]);
    assert.deepEqual(run.errors, []);
  }
  assert.equal(report.runs[0].text, report.runs[1].text);
  assert.ok(report.canceled && report.canceled !== "unexpected success", "active parse must be canceled");
  assert.equal(report.restoredPages, 3);
  assert.deepEqual(report.restoredErrors, []);
  const output = new URL("../../../packages/wasm-web/test-results/render-backpressure/", import.meta.url);
  await mkdir(output, { recursive: true });
  await writeFile(new URL("result.json", output), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ status: "passed", capacities: report.runs.map(run => run.capacity), canceled: report.canceled, restoredPages: report.restoredPages }));
} finally {
  await browser?.close();
  if (server.exitCode === null) {
    const stopped = new Promise(resolve => server.once("exit", resolve));
    server.kill();
    await stopped;
  }
}
