import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

// Use an existing completed document from the production server; never intercept or replace API responses.
const origin = process.argv[2] ?? "http://127.0.0.1:5173";
const id = process.argv[3];
assert(id, "Supply a completed production job");
const output = resolve("target/workbench-web");
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: "chrome", headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
const errors = [];
page.on("pageerror", error => errors.push(error.message));

/** Confirms that long content cannot overlap or vertically scroll the fixed header and tab controls. */
async function assertLayout() {
  const boxes = await page.evaluate(() => {
    const bounds = selector => {
      const node = document.querySelector(selector);
      const rect = node.getBoundingClientRect();
      return { top: rect.top, bottom: rect.bottom, height: rect.height, scroll: node.scrollHeight, client: node.clientHeight };
    };
    return {
      heading: bounds(".inspector-heading"), tabs: bounds(".inspector-tabs"),
      toolbar: bounds(".markdown-toolbar"), body: bounds(".markdown-scroll"), footer: bounds(".inspector-footer"),
    };
  });
  assert(boxes.heading.height >= 70);
  assert(boxes.tabs.height >= 46);
  assert(boxes.tabs.scroll <= boxes.tabs.client + 1, "Tabs must not gain a vertical scrollbar");
  assert(boxes.heading.bottom <= boxes.tabs.top + 1);
  assert(boxes.tabs.bottom <= boxes.toolbar.top + 1);
  assert(boxes.toolbar.bottom <= boxes.body.top + 1);
  assert(boxes.body.height > 180);
  assert(boxes.body.bottom <= boxes.footer.top + 1);
}

try {
  await page.goto(`${origin}/document?job=${encodeURIComponent(id)}&page=9`);
  await page.getByRole("tab", { name: "Markdown", exact: true }).click();
  await page.getByRole("article", { name: "Markdown 预览" }).waitFor({ timeout: 60000 });
  assert.equal(await page.getByRole("tab").count(), 3);
  assert.equal(await page.getByRole("tab", { name: /表格|图片/ }).count(), 0);
  await assertLayout();
  // View switches must retain the current document's parsed HTML and avoid another result request.
  let repeatedMarkdownRequests = 0;
  page.on("request", request => { if (request.url().includes("format=markdown")) repeatedMarkdownRequests++; });
  await page.evaluate(() => { window.reviewPreview = document.querySelector(".markdown-document"); });
  await page.getByRole("button", { name: "源码", exact: true }).click();
  await page.getByRole("button", { name: "预览", exact: true }).click();
  await page.getByRole("tab", { name: "内容", exact: true }).click();
  await page.getByRole("tab", { name: "Markdown", exact: true }).click();
  assert(await page.evaluate(() => window.reviewPreview === document.querySelector(".markdown-document") && window.reviewPreview.isConnected));
  assert.equal(repeatedMarkdownRequests, 0);
  await page.locator(".pdf-pane .canvas-loading").waitFor({ state: "hidden", timeout: 30000 });
  // Observe native canvas allocation in this isolated browser, without adding production test hooks.
  await page.evaluate(() => {
    window.reviewCanvasStarts = 0;
    const property = Object.getOwnPropertyDescriptor(HTMLCanvasElement.prototype, "width");
    Object.defineProperty(HTMLCanvasElement.prototype, "width", { ...property, set(value) {
      if (value > 0 && this.closest(".pdf-pane")) window.reviewCanvasStarts++;
      property.set.call(this, value);
    } });
  });
  const separator = page.getByRole("separator", { name: "调整原文与结果宽度" });
  const initial = await page.locator(".inspector").boundingBox();
  const handle = await separator.boundingBox();
  await page.mouse.move(handle.x + handle.width / 2, handle.y + 100);
  await page.mouse.down();
  for (let step = 1; step <= 30; step++) {
    await page.mouse.move(handle.x + handle.width / 2 - step * 6, handle.y + 100);
    await page.waitForTimeout(16);
  }
  assert.equal(await page.evaluate(() => window.reviewCanvasStarts), 0, "Dragging must reuse the existing PDF bitmap");
  await page.mouse.up();
  await page.waitForFunction(() => document.querySelector(".workbench").dataset.resizing === "false");
  const expanded = await page.locator(".inspector").boundingBox();
  assert(expanded.width > initial.width + 150, "Dragging left must widen the result pane");
  await assertLayout();
  await page.locator(".markdown-scroll").evaluate(node => { node.scrollTop = 10000; });
  await assertLayout();
  await page.locator(".pdf-pane .canvas-loading").waitFor({ state: "hidden", timeout: 30000 });
  await page.waitForFunction(() => window.reviewCanvasStarts > 0);
  const resizeRenders = await page.evaluate(() => window.reviewCanvasStarts);
  assert(resizeRenders <= 2, "Release must commit only the final width, including a possible scrollbar adjustment");
  await page.screenshot({ path: resolve(output, "desktop.png") });

  await separator.focus();
  await separator.press("ArrowRight");
  assert((await page.locator(".inspector").boundingBox()).width < expanded.width - 10);
  await separator.press("End");
  assert(Math.abs((await page.locator(".inspector").boundingBox()).width - 340) < 2);
  await assertLayout();
  await page.screenshot({ path: resolve(output, "narrow-results.png") });
  await separator.press("Home");
  assert(Math.abs((await page.locator(".pdf-pane").boundingBox()).width - 320) < 2);
  await separator.press("Enter");
  assert(Math.abs((await page.locator(".inspector").boundingBox()).width - initial.width) < 2);
  await separator.press("ArrowLeft");
  await separator.dblclick();
  assert(Math.abs((await page.locator(".inspector").boundingBox()).width - initial.width) < 2);

  await page.getByRole("tab", { name: "内容", exact: true }).click();
  await page.locator(".content-block").first().waitFor();
  // The original page content is still available after removing its separate table/image tabs.
  const response = await page.request.get(`${origin}/api/v1/docparse/jobs/result?id=${encodeURIComponent(id)}&page=9`);
  const result = (await response.json()).data.page;
  const images = result.blocks.filter(block => block.image?.delivery.type === "inline").length
    + (result.images ?? []).filter(asset => asset.image.delivery.type === "inline").length;
  assert.equal(await page.locator(".inspector img").count(), images);
  assert.equal(await page.locator(".result-table").count(), result.blocks.filter(block => block.table).length);

  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator(".mobile-view-switch button").last().click();
  assert.equal(await separator.isVisible(), false);
  await page.getByRole("tab", { name: "Markdown", exact: true }).click();
  await page.getByRole("article", { name: "Markdown 预览" }).waitFor();
  await assertLayout();
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
  await page.screenshot({ path: resolve(output, "mobile.png") });
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ status: "passed", job: id, resizeRenders, repeatedMarkdownRequests, screenshots: output }));
} catch (error) {
  await page.screenshot({ path: resolve(output, "failure.png") }).catch(() => {});
  throw error;
} finally {
  await browser.close();
}
