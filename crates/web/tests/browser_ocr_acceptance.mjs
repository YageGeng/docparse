/** Real UI acceptance runner for a Browser skill tab; no fixture backend or hidden application state. */
import { writeFile } from "node:fs/promises";

/** Parses local PDFs through the production controls and yields progress without monopolizing the browser session. */
export async function* runCorpus(tab, manifest, outputDirectory, { inspectAllPages = true } = {}) {
  const results = [];
  for (const input of manifest) {
    const chooser = tab.playwright.waitForEvent("filechooser", { timeoutMs: 10000 });
    await tab.playwright.getByRole("button", { name: "Choose PDF", exact: true }).last().click();
    await (await chooser).setFiles([input.path]);
    await tab.playwright.getByRole("button", { name: "Parse document", exact: true }).click();
    const started = Date.now();
    let observedAt = started;
    let status;
    for (;;) {
      status = await tab.playwright.evaluate(() => ({
        stage: document.querySelector("#stage")?.textContent,
        detail: document.querySelector("#status-detail")?.textContent,
        engine: document.querySelector("#engine-status")?.textContent,
        pages: Number(document.querySelector("#page-count")?.textContent),
      }));
      if (status.stage === "Your document is ready") break;
      if (/Could not|failed|error/i.test(status.stage ?? "")) throw new Error(`${input.file}: ${JSON.stringify(status)}`);
      if (Date.now() - started > 1_800_000) throw new Error(`${input.file}: exceeded 30 minute acceptance deadline`);
      if (Date.now() - observedAt > 15000) { yield { type: "progress", file: input.file, ...status }; observedAt = Date.now(); }
      // Poll the actual parser state while asynchronous model work remains active.
      await new Promise(resolve => setTimeout(resolve, 300));
    }
    if (status.pages !== input.pages || status.engine !== "WebGPU active") throw new Error(`${input.file}: unexpected page count or provider ${JSON.stringify(status)}`);
    // Performance regressions still parse the whole PDF; sampling limits only manual UI inspection.
    const allPages = Array.from({ length: input.pages }, (_, index) => index + 1);
    const samples = new Set(input.pages <= 4 ? allPages : [1, Math.ceil(input.pages / 2), input.pages]);
    const pages = [];
    for (const number of inspectAllPages ? allPages : samples) {
      await tab.playwright.getByRole("button", { name: `Show page ${number}`, exact: true }).click();
      const page = await tab.playwright.evaluate(() => ({
        position: document.querySelector("#page-position")?.textContent,
        warnings: document.querySelector("#page-warnings")?.textContent,
        regions: document.querySelector("#region-count")?.textContent,
      }));
      const blocks = [];
      if (samples.has(number)) {
        const entries = await tab.playwright.evaluate(() => Array.from(document.querySelectorAll("#block-select option"))
          .filter(option => option.value).map(option => ({ id: option.value, label: option.textContent })));
        for (const entry of entries) {
          await tab.playwright.getByRole("combobox", { name: "SELECTED REGION", exact: true }).selectOption(entry.id);
          blocks.push({ ...entry, ...await tab.playwright.evaluate(() => ({
            text: document.querySelector("#extracted-text")?.textContent,
            source: document.querySelector("#selection-meta")?.textContent,
          })) });
        }
      }
      pages.push({ number, ...page, blocks });
      if (Date.now() - observedAt > 15000) {
        yield { type: "inspection", file: input.file, page: number, total: input.pages };
        observedAt = Date.now();
      }
    }
    await tab.playwright.getByRole("button", { name: "Stage timings", exact: true }).click();
    const timings = await tab.playwright.evaluate(() => Array.from(document.querySelectorAll("#timing-rows tr"))
      .map(row => Array.from(row.querySelectorAll("td")).map(cell => cell.textContent)));
    await tab.playwright.getByRole("button", { name: "Close stage timings", exact: true }).click();
    const result = { file: input.file, sha256: input.sha256, ...status, page_count: status.pages, inspected_all_pages: inspectAllPages, pages, timings };
    results.push(result);
    await writeFile(`${outputDirectory}/corpus-results.json`, JSON.stringify(results, null, 2));
    yield { type: "completed", file: input.file, detail: status.detail, inspectedPages: pages.length, warningPages: pages.filter(page => page.warnings).length,
      ocrRuns: timings.find(row => row[0] === "Document · ocr detection inference")?.[1] ?? "0" };
  }
  return results;
}
