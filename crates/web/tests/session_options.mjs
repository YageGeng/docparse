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
  for (const [kind, level, memoryPattern, expected, count] of [
    ["layout", undefined, undefined, "basic", 1],
    ["layout", "level1", true, "basic", 1],
    ["layout", "level2", false, "extended", 1],
    ["layout", "level3", true, "layout", 1],
    ["layout", "all", false, "all", 1],
    ["tsr", "level2", false, "extended", 3],
    ["ocr", "level3", true, "layout", 4],
    ["pp", "level1", true, "basic", 2],
    ["texo", "all", true, "all", 3],
  ]) {
    const models = await page.evaluate(async ({kind, level, memoryPattern}) => {
      const { createParser } = await import("/dist/index.js");
      /** Uses the same verified artifact triplet for each production Paddle model. */
      const artifacts = name => ({ kind: "urls", model: `/models/${name}inference.onnx`, config: `/models/${name}inference.yml`, manifest: `/models/${name}model-manifest.json` });
      const formula = ["pp", "texo"].includes(kind);
      const parser = await createParser({
        executionProvider: "wasm", artifacts: artifacts(""),
        ...(kind === "tsr" ? { tsrArtifacts: artifacts("slanet-plus/"), tsrCellArtifacts: artifacts("rtdetr-table-cell-wireless/") } : {}),
        ...(kind === "ocr" ? { ocrArtifacts: { detection: artifacts("pp-ocrv6-medium-det/"), recognition: artifacts("pp-ocrv6-medium-rec/"), orientation: artifacts("pp-lcnet-textline-ori/") } } : {}),
        config: {
          ...(level === undefined ? {} : { runtime: { optimization_level: level, memory_pattern: memoryPattern } }),
          render: { workers: 1, queue_size: 1 }, layout: { session_size: 1, queue_size: 1 },
          tsr: { session_size: 1, queue_size: 1, mode: kind === "tsr" ? "tsr_only" : "rules_only", cell_detection: { session_size: 1, queue_size: 1 } },
          ocr: { policy: kind === "ocr" ? "always" : "disabled", classify_orientation: true, detection: { session_size: 1, queue_size: 1 }, recognition: { session_size: 1, queue_size: 1 }, orientation: { session_size: 1, queue_size: 1 } },
          formula: { queue_size: 1, engine: { type: formula ? kind : "pp", session_size: 1 }, inline_enabled: formula, display_enabled: formula },
        },
      });
      try { return window.sessionMetrics.models; }
      finally { await parser.close(); }
    }, {kind, level, memoryPattern});
    assert.deepEqual(models.map(model => model.graphOptimizationLevel), Array(count).fill(expected), `${kind}: graph optimizations were not forwarded`);
    assert.deepEqual(models.map(model => model.enableMemPattern), Array(count).fill(memoryPattern ?? false), `${kind}: incorrect memory pattern policy`);
    console.log(JSON.stringify({ kind, level: level ?? "default", graphOptimizationLevel: expected, memoryPattern: memoryPattern ?? false, sessions: count }));
  }
  const rejected = await page.evaluate(async () => {
    const { createParser } = await import("/dist/index.js");
    const codes = [];
    for (const runtime of [
      ...["level0", "disabled", "ALL", 1, null].map(optimization_level => ({optimization_level})),
      ...["true", 1, null].map(memory_pattern => ({memory_pattern})),
    ]) {
      try {
        const parser = await createParser({
          artifacts: { kind: "urls", model: "/unused", config: "/unused", manifest: "/unused" },
          config: {
            runtime, render: { workers: 1, queue_size: 1 },
            layout: { queue_size: 1 }, tsr: { queue_size: 1, mode: "rules_only", cell_detection: { queue_size: 1 } },
            ocr: { detection: { queue_size: 1 }, recognition: { queue_size: 1 }, orientation: { queue_size: 1 } },
            formula: { queue_size: 1, inline_enabled: false, display_enabled: false },
          },
        });
        await parser.close();
        codes.push("unexpected success");
      } catch (error) { codes.push(error.code); }
    }
    return codes;
  });
  assert.deepEqual(rejected, Array(8).fill("InvalidConfig"));
} finally {
  await browser?.close();
  if (server.exitCode === null) {
    const stopped = new Promise(resolve => server.once("exit", resolve));
    server.kill();
    await stopped;
  }
}
