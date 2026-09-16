import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";
import { inspectViewport } from "../../../packages/wasm-web/tests/example.e2e.mjs";

const pdf = resolve(process.argv[2]);
const url = process.argv[3] ?? "http://127.0.0.1:8768/example/";
const output = resolve(process.argv[4] ?? "target/formula-ui");
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: "chrome", headless: true });
const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, permissions: ["clipboard-read", "clipboard-write"] });
const page = await context.newPage();
const formulaRequests = [], errors = [];
page.on("request", request => { if (request.url().includes("/models/pp-formulanet-plus-s/")) formulaRequests.push(request.url()); });
page.on("pageerror", error => errors.push(error.message));
// Observe the real Worker without replacing its inputs, model sessions, or outputs.
await page.addInitScript(() => {
  window.formulaUi = { init: [], documents: [], timings: [], terminated: 0 };
  const NativeWorker = Worker;
  window.Worker = class extends NativeWorker {
    /** Records completed production parses for comparison with the visible inspector. */
    constructor(url, options) {
      super(url, options);
      this.addEventListener("message", ({ data }) => {
        if (data.ok && data.method === "parse") window.formulaUi.documents.push(data.value);
        if (data.event === "timing") window.formulaUi.timings.push(data.value);
      });
    }
    /** Records only the formula switch and artifact presence before forwarding unchanged. */
    postMessage(message, transfer) {
      if (message.method === "init") window.formulaUi.init.push({ inline: message.payload.config.formula.inline_enabled, display: message.payload.config.formula.display_enabled, artifacts: Boolean(message.payload.formulaArtifacts) });
      super.postMessage(message, transfer);
    }
    /** Counts the actual teardown caused by a model-setting change. */
    terminate() { window.formulaUi.terminated++; super.terminate(); }
  };
});

/** Waits for a real UI operation, surfacing model errors and measured progress. */
async function ready(expected) {
  let previous = "";
  const deadline = Date.now() + 600000;
  while (Date.now() < deadline) {
    const stage = await page.locator("#stage").textContent();
    const detail = await page.locator("#status-detail").textContent();
    assert(!/Could not|errors/.test(stage), `${stage}: ${detail}`);
    if (stage !== previous) { console.log(stage, detail); previous = stage; }
    if (stage === expected) return;
    await page.waitForTimeout(1000);
  }
  throw new Error(`Timed out waiting for ${expected}`);
}

try {
  await page.goto(url);
  assert(await page.getByRole("switch", { name: "Inline formulas", exact: true }).isChecked());
  assert(await page.getByRole("switch", { name: "Display formulas", exact: true }).isChecked());
  await inspectViewport(page);
  await page.locator("#prepare").click();
  assert(await page.locator("#inline-formula-enabled").isDisabled());
  assert(await page.locator("#display-formula-enabled").isDisabled());
  await ready("Models ready");
  assert.equal(await page.locator("#engine-status").getAttribute("data-provider"), "webgpu");
  assert.deepEqual(await page.evaluate(() => window.formulaUi.init), [{ inline: true, display: true, artifacts: true }]);
  assert.equal(formulaRequests.length, 3, "All formula assets must be served by the production example");
  await page.locator("#file").setInputFiles(pdf);
  await page.locator("#parse").click();
  await ready("Your document is ready");
  const document = await page.evaluate(() => window.formulaUi.documents[0]);
  const coldTimings = await page.evaluate(() => window.formulaUi.timings);
  assert.equal(document.errors.length, 0);
  if (pdf.endsWith("2410.05779v3.pdf")) {
    const paragraph = document.pages.find(page => page.page_number === 9).blocks.find(block => block.text.includes("In this context"));
    assert(paragraph.markdown && !paragraph.markdown.includes("\nextract repre-"), "A subscript split across source lines must not appear twice");
  }
  const formulas = document.pages.flatMap(page => (page.formulas ?? []).map(formula => ({ ...formula, page: page.page_number })));
  assert(formulas.length > 0);
  assert(formulas.every(formula => formula.latex && formula.markdown && !formula.error));
  assert(formulas.every(formula => formula.engine === "pp-formulanet-plus-s-onnx-webgpu"));
  // Exercise both inline and display formulas where present, including their real clipboard actions.
  for (const label of new Set(formulas.map(formula => formula.label))) {
    const formula = formulas.find(formula => formula.label === label && formula.block_id);
    assert(formula, `Missing source anchor for ${label}`);
    await page.getByRole("button", { name: `Show page ${formula.page}`, exact: true }).click();
    await page.locator("#block-select").selectOption(formula.block_id);
    const block = document.pages.find(page => page.page_number === formula.page).blocks.find(block => block.id === formula.block_id);
    const details = page.locator("#formula-results > details");
    if (block.markdown) {
      assert((await page.locator("#extracted-text p .katex").count()) > 0, "Math must appear inside the paragraph");
      assert.equal(await page.locator("#extracted-text .katex-display").count(), 0);
      assert.equal(await details.evaluate(node => node.open), false);
      await page.locator("#copy").click();
      assert.equal(await page.evaluate(() => navigator.clipboard.readText()), block.markdown);
      if (block.text.includes("Data Indexer")) {
        assert((await page.locator("#extracted-text").innerText()).includes("Data Indexer"));
        assert((await page.locator("#extracted-text").innerText()).includes("which involves"));
      }
    }
    if (!(await details.evaluate(node => node.open))) await details.locator("summary").click();
    const card = page.locator(`[data-formula-id="${formula.id}"]`);
    assert.equal(await card.locator("pre").count(), 0, "Source must not replace the rendered formula");
    for (const format of ["latex", "markdown"]) {
      assert.equal(await card.locator(`[data-format="${format}"] .katex`).count(), 1);
      assert(await card.locator(`[data-format="${format}"] .katex-html`).isVisible());
    }
    for (const [label, expected] of [["LaTeX", formula.latex], ["Markdown", formula.markdown]]) {
      await card.getByRole("button", { name: `Copy ${label}`, exact: true }).click();
      assert.equal(await page.evaluate(() => navigator.clipboard.readText()), expected);
    }
    if (block.markdown) await details.locator("summary").click();
  }
  await inspectViewport(page);
  await page.screenshot({ path: resolve(output, "enabled.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  await inspectViewport(page);
  assert(await page.locator("#formula-results").isVisible());
  await page.screenshot({ path: resolve(output, "narrow.png") });
  await page.setViewportSize({ width: 1440, height: 1000 });
  // Reuse the same prepared sessions to separate warm inference from initial kernel setup.
  const warmStart = await page.evaluate(() => window.formulaUi.timings.length);
  await page.locator("#parse").click();
  await ready("Your document is ready");
  const warmTimings = await page.evaluate(offset => window.formulaUi.timings.slice(offset), warmStart);
  assert.deepEqual(await page.evaluate(() => window.formulaUi.documents[1].pages.flatMap(page => page.formulas ?? []).map(formula => formula.latex)), formulas.map(formula => formula.latex));
  const requestsBeforeOff = formulaRequests.length;
  await page.locator("#display-formula-enabled").uncheck();
  assert(await page.locator("#inline-formula-enabled").isChecked());
  assert(await page.locator("#inline-formula-enabled").isEnabled());
  await page.locator("#inline-formula-enabled").uncheck();
  assert.equal(await page.locator(".formula-result").count(), 0);
  assert(await page.locator("#prepare").isEnabled());
  assert.equal(await page.evaluate(() => window.formulaUi.terminated), 1);
  const timingCount = await page.evaluate(() => window.formulaUi.timings.length);
  await page.locator("#prepare").click();
  await ready("Models ready");
  await page.locator("#parse").click();
  await ready("Your document is ready");
  assert.equal(formulaRequests.length, requestsBeforeOff, "Disabled recognition must not download formula assets");
  assert.deepEqual(await page.evaluate(() => window.formulaUi.init[1]), { inline: false, display: false, artifacts: false });
  assert.equal(await page.evaluate(() => window.formulaUi.documents[2].pages.flatMap(page => page.formulas ?? []).length), 0);
  const disabledTimings = await page.evaluate(offset => window.formulaUi.timings.slice(offset), timingCount);
  assert.equal(disabledTimings.some(timing => timing.stage.startsWith("formula_")), false);
  await page.locator("#display-formula-enabled").check();
  assert.equal(await page.locator("#inline-formula-enabled").isChecked(), false);
  await page.locator("#inline-formula-enabled").check();
  assert(await page.locator("#prepare").isEnabled());
  assert.equal(await page.evaluate(() => window.formulaUi.terminated), 2);
  assert.deepEqual(errors, []);
  const report = { status: "passed", browser: browser.version(), model: "pp-formulanet-plus-s", pages: document.pages.length, formulas: formulas.length, labels: [...new Set(formulas.map(formula => formula.label))], formulaRequests, checks: ["default-on", "independent-switches", "real-webgpu", "latex-markdown-inspector", "clipboard", "narrow-viewport", "warm-output-parity", "off-no-download-or-inference", "setting-change-rebuild"] };
  await writeFile(resolve(output, "summary.json"), JSON.stringify(report, null, 2) + "\n");
  await writeFile(resolve(output, "timings.json"), JSON.stringify({ cold: coldTimings, warm: warmTimings, disabled: disabledTimings }, null, 2) + "\n");
  console.log(JSON.stringify(report));
} catch (error) {
  await page.screenshot({ path: resolve(output, "failure.png") }).catch(() => {});
  throw error;
} finally { await browser.close(); }
