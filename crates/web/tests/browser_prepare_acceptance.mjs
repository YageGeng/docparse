/** Real browser preparation and reuse checks against the production example and its configured models. */
import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";

/** Waits for visible completion, yielding progress so slow real initialization remains observable. */
async function* waitForStage(tab, expected) {
  const started = Date.now();
  let observedAt = started;
  for (;;) {
    const state = await tab.playwright.evaluate(() => ({
      stage: document.querySelector("#stage")?.textContent,
      detail: document.querySelector("#status-detail")?.textContent,
      engine: document.querySelector("#engine-status")?.textContent,
      pages: Number(document.querySelector("#page-count")?.textContent),
      prepareDisabled: document.querySelector("#prepare")?.disabled,
      busy: !document.querySelector("#cancel")?.hidden,
    }));
    if (state.stage === expected && !state.busy) return state;
    assert(!/Could not|failed|errors|canceled/i.test(state.stage ?? ""), JSON.stringify(state));
    assert(Date.now() - started < 600_000, `Timed out waiting for ${expected}`);
    if (Date.now() - observedAt > 15000) { yield { type: "progress", ...state }; observedAt = Date.now(); }
    await new Promise(resolve => setTimeout(resolve, 300));
  }
}

/** Reads the user-visible timing snapshot without accessing parser internals. */
async function readTimings(tab) {
  await tab.playwright.getByRole("button", { name: "Stage timings", exact: true }).click();
  const rows = await tab.playwright.evaluate(() => Array.from(document.querySelectorAll("#timing-rows tr"))
    .map(row => Array.from(row.querySelectorAll("td")).map(cell => cell.textContent)));
  await tab.playwright.getByRole("button", { name: "Close stage timings", exact: true }).click();
  return rows;
}

/** Exercises all OCR/TSR switch combinations, CPU preparation, cancellation and reusable sessions with a real PDF. */
export async function* runPreparation(tab, pdf, outputDirectory, { providers = ["webgpu", "wasm"] } = {}) {
  const results = [];
  const cases = [
    { provider: "webgpu", tables: "fallback", ocr: "missing_regions", models: "layout + TSR + OCR" },
    { provider: "webgpu", tables: "rules_only", ocr: "disabled", models: "layout", cancel: true },
    { provider: "webgpu", tables: "fallback", ocr: "disabled", models: "layout + TSR" },
    { provider: "webgpu", tables: "rules_only", ocr: "missing_regions", models: "layout + OCR" },
    { provider: "wasm", tables: "fallback", ocr: "missing_regions", models: "layout + TSR + OCR" },
  ];
  // Provider filtering lets an interrupted browser run resume without repeating completed GPU checks.
  for (const test of cases.filter(test => providers.includes(test.provider))) {
    await tab.playwright.getByRole("combobox", { name: "Inference engine", exact: true }).selectOption(test.provider);
    await tab.playwright.getByRole("combobox", { name: "Tables", exact: true }).selectOption(test.tables);
    await tab.playwright.getByRole("combobox", { name: "OCR", exact: true }).selectOption(test.ocr);
    await tab.playwright.getByRole("button", { name: "Prepare models", exact: true }).click();
    if (test.cancel) {
      await tab.playwright.getByRole("button", { name: "Cancel preparation", exact: true }).click();
      const canceled = yield* waitForStage(tab, "Preparation canceled");
      assert.equal(canceled.prepareDisabled, false);
      await tab.playwright.getByRole("button", { name: "Prepare models", exact: true }).click();
      // File selection while preparation is pending must preserve that initialization.
      const chooser = tab.playwright.waitForEvent("filechooser", { timeoutMs: 10000 });
      await tab.playwright.getByRole("button", { name: "Choose PDF", exact: true }).last().click();
      await (await chooser).setFiles([pdf]);
    }
    const prepared = yield* waitForStage(tab, "Models ready");
    assert(prepared.detail.startsWith(`${test.models} initialized.`), JSON.stringify(prepared));
    assert.equal(prepared.pages, 0, "Preparation must not parse a document");
    assert.equal(prepared.prepareDisabled, true);
    assert.equal(prepared.engine, test.provider === "webgpu" ? "WebGPU active" : "CPU active");
    const preparation = await readTimings(tab);
    assert(preparation.length > 0 && preparation.every(row => row[0].startsWith("Preparation · ")));
    assert.equal(preparation.find(row => row[0] === "Preparation · model init")?.[1], "1");
    yield { type: "prepared", ...test, ...prepared };

    // Selecting and parsing twice must keep the same initialization observations, including on CPU.
    for (let repetition = 0; repetition < 2; repetition++) {
      const chooser = tab.playwright.waitForEvent("filechooser", { timeoutMs: 10000 });
      await tab.playwright.getByRole("button", { name: "Choose PDF", exact: true }).last().click();
      await (await chooser).setFiles([pdf]);
      // Native file inputs need not emit change for the same file; parse itself resets document timings.
      const selectedTimings = await readTimings(tab);
      assert.deepEqual(selectedTimings.filter(row => row[0].startsWith("Preparation · ")), preparation);
      if (repetition === 0) assert.deepEqual(selectedTimings, preparation);
      await tab.playwright.getByRole("button", { name: "Parse document", exact: true }).click();
      const parsed = yield* waitForStage(tab, "Your document is ready");
      assert.equal(parsed.pages, 1);
      assert.equal(parsed.engine, prepared.engine);
      const timings = await readTimings(tab);
      assert.deepEqual(timings.filter(row => row[0].startsWith("Preparation · ")), preparation);
      assert.equal(timings.some(row => row[0] === "Document · ocr detection inference"), test.ocr !== "disabled");
      const entries = await tab.playwright.evaluate(() => Array.from(document.querySelectorAll("#block-select option"))
        .filter(option => option.value).map(option => option.value));
      const texts = [];
      for (const id of entries) {
        await tab.playwright.getByRole("combobox", { name: "SELECTED REGION", exact: true }).selectOption(id);
        texts.push(await tab.playwright.evaluate(() => document.querySelector("#extracted-text")?.textContent));
      }
      assert(texts.some(text => text.includes("Mixed native labels and raster values")));
      if (test.ocr !== "disabled") {
        for (const text of ["Total: 100", "Pay USD 20", "Hello, world"]) assert(texts.includes(text), JSON.stringify(texts));
      }
      results.push({ ...test, repetition, prepared, parsed, texts, timings });
      await writeFile(`${outputDirectory}/preparation-results.json`, JSON.stringify(results, null, 2));
      yield { type: "parsed", ...test, repetition, ...parsed };
    }
  }
  return results;
}
