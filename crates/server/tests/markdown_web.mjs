import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

// Exercise the production API and an existing real parse; no fixture backend or intercepted responses.
const origin = process.argv[2] ?? "http://127.0.0.1:5173";
const id = process.argv[3];
assert(id, "Supply a completed production job containing formulas and tables");
const output = resolve("target/markdown-web");
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: "chrome", headless: true });
const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, permissions: ["clipboard-read", "clipboard-write"] });
const page = await context.newPage();
const errors = [];
page.on("pageerror", error => errors.push(error.message));
try {
  const url = `${origin}/api/v1/docparse/jobs/result?id=${encodeURIComponent(id)}&format=markdown`;
  const response = await page.request.get(url);
  assert.equal(response.status(), 200);
  assert.match(response.headers()["content-type"], /^text\/markdown/);
  const source = await response.text();
  assert(source.length > 0);
  const cached = await page.request.get(url, { headers: { "If-None-Match": response.headers().etag } });
  assert.equal(cached.status(), 304);
  const json = await page.request.get(`${origin}/api/v1/docparse/jobs/result?id=${encodeURIComponent(id)}&page=1`);
  assert.equal(json.status(), 200);
  assert((await json.json()).data.page, "JSON must remain available alongside Markdown");
  await page.goto(`${origin}/document?job=${encodeURIComponent(id)}`);
  await page.getByRole("tab", { name: "Markdown", exact: true }).click();
  const preview = page.getByRole("article", { name: "Markdown 预览" });
  await preview.waitFor({ timeout: 60000 });
  assert(await preview.locator(".katex").count() > 0, "Real formulas must render");
  assert(await preview.locator("table").count() > 0, "Real tables must render");
  assert.equal(await preview.locator("script, iframe").count(), 0);
  await page.getByRole("button", { name: "源码", exact: true }).click();
  assert.equal(await page.locator(".markdown-source").textContent(), source);
  await page.getByRole("button", { name: "复制 Markdown", exact: true }).click();
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), source);
  const downloadEvent = page.waitForEvent("download");
  await page.locator(".markdown-toolbar").getByRole("link", { name: "下载当前 Markdown" }).click();
  const download = await downloadEvent;
  assert(download.suggestedFilename().endsWith(".md"));
  assert.equal(await readFile(await download.path(), "utf8"), source);
  await page.getByRole("button", { name: "预览", exact: true }).click();
  await page.screenshot({ path: resolve(output, "desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator(".mobile-view-switch button").last().click();
  assert(await preview.isVisible());
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), "Mobile layout must not overflow");
  await page.screenshot({ path: resolve(output, "mobile.png") });
  // Check the renderer's trust boundary separately without replacing any real API response.
  const safe = await page.evaluate(async () => {
    const { renderMarkdownDocument } = await import("/src/lib/math.ts");
    const node = document.createElement("div");
    node.innerHTML = renderMarkdownDocument('# Title\n\n<script>window.bad=1</script>\n\n<table onclick="bad()"><tr><td rowspan="2" style="background:url(https://invalid.example/track)">$x^2$<br>safe<img src="https://invalid.example/track" onerror="bad()"></td></tr></table>\n\n[bad](javascript:alert(1))\n\n![image](https://invalid.example/track)\n\n$\\unknowncommand{x}$ after');
    return {
      unsafe: node.querySelectorAll("script, img, iframe, [onclick], [onerror], td[style], a[href^='javascript:']").length,
      span: node.querySelector("td")?.getAttribute("rowspan"),
      math: node.querySelectorAll("td .katex").length,
      breaks: node.querySelectorAll("td br").length,
      fallback: node.querySelectorAll(".formula-render-error").length,
      text: node.textContent,
    };
  });
  assert.equal(safe.unsafe, 0);
  assert.equal(safe.span, "2");
  assert.equal(safe.math, 1);
  assert.equal(safe.breaks, 1);
  assert.equal(safe.fallback, 1);
  assert(safe.text.includes("after"));
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ status: "passed", job: id, markdownCharacters: source.length, screenshots: output }));
} catch (error) {
  await page.screenshot({ path: resolve(output, "failure.png") }).catch(() => {});
  throw error;
} finally {
  await browser.close();
}
