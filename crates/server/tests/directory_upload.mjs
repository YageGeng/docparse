// Exercise recursive directory selection against the configured production backend, without response fixtures.
import assert from "node:assert/strict";
import { copyFile, mkdir, mkdtemp, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium } from "../../../packages/wasm-web/node_modules/playwright/index.mjs";

const origin = process.argv[2] ?? "http://127.0.0.1:5173";
const output = resolve(process.argv[3] ?? "target/directory-upload-review");
const temporary = await mkdtemp(join(tmpdir(), "docparse-directory-"));
const folder = join(temporary, "documents");
const empty = join(temporary, "no-pdfs");
const fixture = new URL("../../core/tests/fixtures/pdf/extraction_metadata.pdf", import.meta.url);
const fileSize = (await stat(fixture)).size;
const paths = ["report.pdf", "nested/report.pdf", "nested/deeper/UPPER.PDF"];
const browser = await chromium.launch({ channel: "chrome", headless: true });
let page;
try {
  await mkdir(output, { recursive: true });
  await mkdir(join(folder, "nested/deeper"), { recursive: true });
  await mkdir(empty);
  for (const path of paths) await copyFile(fixture, join(folder, path));
  await writeFile(join(folder, "notes.txt"), "Ignore this non-PDF file.");
  // Keep the original size so later reselection exercises the same durable identity with a repaired PDF.
  await writeFile(join(folder, "broken.pdf"), Buffer.alloc(fileSize, "x"));
  await writeFile(join(empty, "notes.txt"), "This directory contains no PDFs.");
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors = [];
  const responses = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("response", response => {
    if (response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/jobs")) responses.push(response);
  });
  // Fail before uploading if the configured production service is unavailable.
  const health = await page.request.get(`${origin}/api/v1/docparse/health`);
  assert.equal(health.status(), 200, "The configured production backend must be available");
  assert.equal((await health.json()).success, true);
  await page.goto(origin);
  const picker = page.getByLabel("选择包含 PDF 的目录", { exact: true });
  assert(await picker.evaluate(input => input.webkitdirectory), "The input must select a directory hierarchy");
  await picker.setInputFiles(empty);
  await page.getByRole("status").filter({ hasText: "此目录及其子目录中没有 PDF 文件。" }).waitFor();
  assert.equal(responses.length, 0);

  // Activate the visible button so the native chooser and keyboard-accessible control share the same path.
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "选择目录", exact: true }).click();
  await (await chooser).setFiles(folder);
  const queue = page.getByLabel("上传队列", { exact: true });
  await queue.getByText("请选择有效的 PDF 文件。", { exact: true }).waitFor();
  await page.waitForFunction(() => {
    return !document.querySelector('input[aria-label="选择包含 PDF 的目录"]').disabled;
  }, undefined, { timeout: 120000 });
  assert.equal(await queue.getByRole("listitem").count(), 1, "Acknowledged uploads must disappear immediately");
  for (const path of paths) assert.equal(await queue.getByLabel(`上传 documents/${path}`, { exact: true }).count(), 0);
  assert.equal(responses.length, 3, "Invalid PDFs and other file types must not reach the backend");
  const jobs = [];
  for (const response of responses) {
    assert.equal(response.status(), 202);
    const body = await response.json();
    assert.equal(body.success, true);
    jobs.push(body.data.id);
  }
  assert.equal(new Set(jobs).size, 3, "Same-named files need independent submission identities");
  assert.equal(await picker.inputValue(), "", "The same directory can be selected again");
  await page.screenshot({ path: join(output, "desktop.png") });
  await page.setViewportSize({ width: 375, height: 812 });
  assert(await page.getByRole("button", { name: "选择目录", exact: true }).isVisible());
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "Directory controls must fit mobile widths");
  const pathLayout = await queue.getByLabel("上传 documents/broken.pdf", { exact: true }).locator("p").first().evaluate(node => ({
    height: node.getBoundingClientRect().height, lineHeight: Number.parseFloat(getComputedStyle(node).lineHeight),
  }));
  assert(pathLayout.height <= pathLayout.lineHeight * 2, "Action buttons must not squeeze a short path into a vertical column");
  await page.screenshot({ path: join(output, "mobile.png") });
  // Recreate lost acknowledgements using real saved jobs, then let the production status API reconcile them.
  await page.evaluate(({ jobs, fileSize }) => {
    const key = "docparse.pending-upload:/api/v1/docparse";
    const pending = JSON.parse(localStorage.getItem(key));
    localStorage.setItem(key, JSON.stringify([...pending, ...jobs.map((id, index) => ({ id, name: `saved-${index}.pdf`, size: fileSize }))]));
  }, { jobs, fileSize });
  await page.reload();
  const failed = page.getByLabel("上传 documents/broken.pdf", { exact: true });
  await failed.getByRole("button", { name: "重新选择文件", exact: true }).waitFor();
  assert.equal(await queue.getByRole("listitem").count(), 1, "Saved jobs must be removed by automatic recovery");
  const pending = await page.evaluate(() => JSON.parse(localStorage.getItem("docparse.pending-upload:/api/v1/docparse")));
  assert.equal(pending.length, 1);

  // A transport failure keeps the status unknown; it must not invite re-uploading a saved document.
  await page.evaluate(({ id, fileSize }) => {
    localStorage.setItem("docparse.pending-upload:/api/v1/docparse", JSON.stringify([{ id, name: "saved.pdf", size: fileSize }]));
  }, { id: jobs[0], fileSize });
  await page.route("**/api/v1/docparse/jobs/status?*", route => route.abort("internetdisconnected"));
  await page.reload();
  const uncertain = page.getByLabel("上传 saved.pdf", { exact: true });
  await uncertain.getByRole("alert").waitFor();
  await uncertain.getByRole("button", { name: "检查任务", exact: true }).waitFor();
  assert.equal(await uncertain.getByRole("button", { name: "重新选择文件", exact: true }).count(), 0);
  await page.unroute("**/api/v1/docparse/jobs/status?*");
  await uncertain.getByRole("button", { name: "检查任务", exact: true }).click();
  await queue.waitFor({ state: "detached" });

  // A confirmed missing file retains its original identity when reselected, then clears the last row on success.
  await page.evaluate(pending => localStorage.setItem("docparse.pending-upload:/api/v1/docparse", JSON.stringify(pending)), pending);
  await page.reload();
  await failed.getByRole("button", { name: "重新选择文件", exact: true }).waitFor();
  await copyFile(fixture, join(folder, "broken.pdf"));
  const retriedResponse = page.waitForResponse(response => response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/jobs"));
  const retryChooser = page.waitForEvent("filechooser");
  await failed.getByRole("button", { name: "重新选择文件", exact: true }).click();
  await (await retryChooser).setFiles(join(folder, "broken.pdf"));
  const retried = await retriedResponse;
  assert.equal(retried.status(), 202);
  assert.equal((await retried.json()).data.id, pending[0].id);
  await queue.waitFor({ state: "detached" });
  assert.equal(await page.evaluate(() => localStorage.getItem("docparse.pending-upload:/api/v1/docparse")), null);
  assert.deepEqual(errors, []);
  const report = { status: "passed", uploaded: jobs.length + 1, nestedPaths: paths, jobs: [...jobs, pending[0].id], invalidPdfRejected: true, emptyDirectory: true, recoveredRelativePath: true, restoredSuccessRemoved: true, unknownStatusPreserved: true, reselectionClearsQueue: true };
  await writeFile(join(output, "summary.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify(report));
} catch (error) {
  if (page && page.url() !== "about:blank") {
    await page.screenshot({ path: join(output, "failure.png") }).catch(() => {});
    console.error(await page.getByLabel("上传文档", { exact: true }).innerText({ timeout: 3000 }).catch(() => "Upload panel unavailable"));
  }
  throw error;
} finally {
  await browser.close();
  await rm(temporary, { recursive: true, force: true });
}
