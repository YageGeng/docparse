import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

const pdf = resolve(process.argv[2]);
const origin = process.argv[3] ?? "http://127.0.0.1:5173";
const output = resolve(process.argv[4] ?? "target/formula-render-review/http");
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: "chrome", headless: true });
const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, permissions: ["clipboard-read", "clipboard-write"] });
const page = await context.newPage();
const errors = [];
page.on("pageerror", error => errors.push(error.message));
try {
  await page.goto(origin);
  const uploaded = page.waitForResponse(response => response.url().endsWith("/api/v1/docparse/jobs") && response.request().method() === "POST");
  await page.getByLabel("选择 PDF 文件", { exact: true }).setInputFiles(pdf);
  const response = await uploaded;
  assert.equal(response.status(), 202);
  const job = (await response.json()).data;
  console.log("Uploaded real document", job.id);
  let snapshot;
  const deadline = Date.now() + 600000;
  while (Date.now() < deadline) {
    snapshot = (await (await page.request.get(`${origin}/api/v1/docparse/jobs/status?id=${job.id}`)).json()).data;
    assert.notEqual(snapshot.status, "failed", JSON.stringify(snapshot.error));
    if (snapshot.status === "succeeded") break;
    await page.waitForTimeout(2000);
  }
  assert.equal(snapshot.status, "succeeded", "Production parser timed out");
  const document = (await (await page.request.get(`${origin}/api/v1/docparse/jobs/result?id=${job.id}`)).json()).data;
  await writeFile(resolve(output, "result.json"), JSON.stringify(document));
  assert.equal(document.errors.length, 0);
  if (pdf.endsWith("2410.05779v3.pdf")) {
    const paragraph = document.pages.find(page => page.page_number === 9).blocks.find(block => block.text.includes("In this context"));
    assert(paragraph.markdown && !paragraph.markdown.includes("\nextract repre-"), "A subscript split across source lines must not appear twice");
  }
  const formulas = document.pages.flatMap(page => page.formulas ?? []);
  assert(formulas.length > 0);
  assert(formulas.every(formula => formula.latex && formula.markdown && !formula.error));
  let inspected = 0;
  for (const resultPage of document.pages.filter(page => page.formulas?.length)) {
    const formula = resultPage.formulas[0];
    await page.goto(`${origin}/document?job=${job.id}&page=${resultPage.page_number}${formula.block_id ? `&block=${encodeURIComponent(formula.block_id)}` : ""}`);
    await page.locator(`[data-formula-id="${formula.id}"]`).waitFor({ state: "attached", timeout: 30000 });
    const selectedBlock = resultPage.blocks.find(block => block.id === formula.block_id);
    if (selectedBlock?.markdown) {
      const prose = page.locator(`[id="content-${selectedBlock.id}"] .inline-prose`);
      assert((await prose.locator("p .katex").count()) > 0, "Math must appear inside the paragraph");
      assert.equal(await prose.locator(".katex-display").count(), 0);
      await page.getByRole("button", { name: "复制内容", exact: true }).click();
      assert.equal(await page.evaluate(() => navigator.clipboard.readText()), selectedBlock.markdown);
      if (selectedBlock.text.includes("Data Indexer")) {
        assert((await prose.innerText()).includes("Data Indexer"));
        assert((await prose.innerText()).includes("which involves"));
      }
    }
    for (const formula of resultPage.formulas) {
      const card = page.locator(`[data-formula-id="${formula.id}"]`);
      const details = card.locator("xpath=ancestor::details");
      if (await details.count() && !(await details.evaluate(node => node.open))) await details.locator("summary").click();
      for (const [format, label] of [["latex", "LaTeX"], ["markdown", "Markdown"]]) {
        const preview = card.locator(`[data-format="${format}"]`);
        assert.equal(await preview.locator(".katex").count(), 1, `${formula.id} ${format} must render`);
        await card.getByRole("button", { name: `复制 ${label} 源码`, exact: true }).click();
        assert.equal(await page.evaluate(() => navigator.clipboard.readText()), formula[format]);
      }
      inspected++;
    }
    if (formula.block_id) {
      await page.getByRole("tab", { name: "JSON", exact: true }).click();
      const selected = JSON.parse(await page.locator(".json-view").textContent());
      assert(selected.formulas.some(value => value.id === formula.id), "Selected JSON must retain formula source");
      await page.getByRole("tab", { name: "内容", exact: true }).click();
    }
    for (const details of await page.locator("details.formula-details[open]").all()) await details.locator("summary").click();
  }
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: resolve(output, "desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator('.mobile-view-switch button').last().click();
  assert(await page.locator(".inspector").isVisible());
  await page.screenshot({ path: resolve(output, "mobile.png") });
  assert.deepEqual(errors, []);
  const report = { status: "passed", job: job.id, browser: browser.version(), pages: document.pages.length, formulas: formulas.length, renderedAndCopied: inspected, engines: [...new Set(formulas.map(formula => formula.engine))] };
  await writeFile(resolve(output, "summary.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify(report));
} catch (error) {
  await page.screenshot({ path: resolve(output, "failure.png") }).catch(() => {});
  throw error;
} finally { await browser.close(); }
