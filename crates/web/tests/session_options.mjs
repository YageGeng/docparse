// Verify production model builders forward optimization policies to real ORT Web sessions.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

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
  const recorder = await readFile(new URL("../../../packages/wasm-web/tests/instrumented-worker.js", import.meta.url), "utf8");
  await page.route("**/__session-observe.js?*", route => route.fulfill({ contentType: "text/javascript", body: recorder }));
  await page.addInitScript(() => {
    const NativeWorker = Worker;
    window.Worker = class extends NativeWorker {
      /** Observes the real runtime without replacing model construction or options. */
      constructor(url, options) {
        const entry = new URL("/__session-observe.js", location.href);
        entry.searchParams.set("worker", String(url));
        super(entry, options);
        this.addEventListener("message", ({ data }) => { if (data.metrics) window.sessionMetrics = data.metrics; });
      }
    };
  });
  await page.goto(`${origin}/example/`);
  for (const [kind, patterns] of [
    ["layout", [true]], ["tsr", [true, true, true]],
    ["ocr", [true, false, false, false]], ["pp", [true, false]], ["texo", [true, false, false]],
  ]) {
    const models = await page.evaluate(async kind => {
      const { createParser } = await import("/dist/index.js");
      /** Uses the same verified artifact triplet for each production Paddle model. */
      const artifacts = name => ({ kind: "urls", model: `/models/${name}inference.onnx`, config: `/models/${name}inference.yml`, manifest: `/models/${name}model-manifest.json` });
      const formula = ["pp", "texo"].includes(kind);
      const parser = await createParser({
        executionProvider: "wasm", artifacts: artifacts(""),
        ...(kind === "tsr" ? { tsrArtifacts: artifacts("slanet-plus/"), tsrCellArtifacts: artifacts("rtdetr-table-cell-wireless/") } : {}),
        ...(kind === "ocr" ? { ocrArtifacts: { detection: artifacts("pp-ocrv6-medium-det/"), recognition: artifacts("pp-ocrv6-medium-rec/"), orientation: artifacts("pp-lcnet-textline-ori/") } } : {}),
        config: {
          render: { workers: 1, queue_size: 1 }, layout: { session_size: 1, queue_size: 1 },
          tsr: { session_size: 1, queue_size: 1, mode: kind === "tsr" ? "tsr_only" : "rules_only", cell_detection: { session_size: 1, queue_size: 1 } },
          ocr: { policy: kind === "ocr" ? "always" : "disabled", classify_orientation: true, detection: { session_size: 1, queue_size: 1 }, recognition: { session_size: 1, queue_size: 1 }, orientation: { session_size: 1, queue_size: 1 } },
          formula: { queue_size: 1, engine: { type: formula ? kind : "pp", session_size: 1 }, inline_enabled: formula, display_enabled: formula },
        },
      });
      try { return window.sessionMetrics.models; }
      finally { await parser.close(); }
    }, kind);
    assert.deepEqual(models.map(model => model.graphOptimizationLevel), patterns.map(() => "all"), `${kind}: graph optimizations were not forwarded`);
    assert.deepEqual(models.map(model => model.enableMemPattern), patterns, `${kind}: incorrect memory pattern policy`);
    console.log(JSON.stringify({ kind, graphOptimizationLevel: "all", memoryPatterns: patterns }));
  }
} finally {
  await browser?.close();
  if (server.exitCode === null) {
    const stopped = new Promise(resolve => server.once("exit", resolve));
    server.kill();
    await stopped;
  }
}
