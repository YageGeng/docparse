import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

/** Checks supplied-input transport against the real SDK and layout backend; it does not evaluate any TSR model. */
export async function runTableInputChecks(page, { pdfPath, baseUrl = 'http://127.0.0.1:8768' }) {
  const bytes = [...await readFile(pdfPath)];
  await page.goto(`${baseUrl}/example/`);
  const report = await page.evaluate(async ({ bytes, baseUrl }) => {
    const { createParser } = await import(`${baseUrl}/dist/index.js`);
    const parser = await createParser({
      executionProvider: 'webgpu', allowCpuFallback: true,
      artifacts: { kind: 'urls', model: `${baseUrl}/models/inference.onnx`, config: `${baseUrl}/models/inference.yml`, manifest: `${baseUrl}/models/model-manifest.json` },
    });
    const pdf = new Uint8Array(bytes);
    /** Compares layout boundaries and source ownership independently of derived block/cell ordering. */
    const facts = doc => JSON.stringify([
      doc.pages.flatMap(p => p.blocks.map(b => [b.id, b.bbox, b.polygon, b.source_region, b.source_regions])).sort((a,b) => a[0].localeCompare(b[0])),
      doc.pages.flatMap(p => p.blocks.flatMap(b => b.lines.flatMap(l => l.text_items.map(i => [b.id, i.id, i.raw_text, i.bbox, i.provenance])))).sort((a,b) => a[1].localeCompare(b[1])),
    ]);
    /** Lists only the regions found by the production layout model. */
    const tables = doc => doc.pages.flatMap(p => p.blocks.filter(b => b.label === 'table'));
    /** Structure-only input cannot fill a region lacking both native and OCR source text. */
    const hasText = block => block.lines.some(line => line.text_items.some(item => item.raw_text.trim().length > 0));
    /** A caller-declared topology exercises the input API, with no claim that a model inferred it. */
    const supplied = request => ({ request_id: request.request_id, structure_tokens: ['<tr>', '<td></td>', '</tr>'], cell_bboxes: [[0, 0, request.image.width, request.image.height]] });
    /** Fails the contract check with enough context for a deterministic report. */
    const check = (condition, message) => { if (!condition) throw new Error(message); };
    const cases = [];
    try {
      const baseline = await parser.parse(pdf);
      const baselineFacts = facts(baseline);
      const count = tables(baseline).length;
      const textless = tables(baseline).filter(b => !hasText(b)).length;
      const failures = tables(baseline).filter(b => !b.table).length;
      check(count > 0, 'Input PDF must have layout-detected table regions');
      check(failures > 0, 'Input PDF must include a locally unresolved table to exercise fallback');
      let requested = [];
      const fallback = await parser.parse(pdf, { table: { mode: 'fallback' }, onTableStructure: async request => { requested.push(request.request_id); return supplied(request); } });
      check(requested.length === failures, 'Fallback called external input for a locally successful table');
      check(tables(fallback).every(b => Boolean(b.table) === hasText(b)), `Supplied fallback structures did not preserve source availability: ${JSON.stringify(fallback.pages.flatMap(p => p.warnings))}`);
      check(facts(fallback) === baselineFacts, 'Fallback changed canonical source facts');
      cases.push({ name: 'fallback', requested: requested.length, tables: count, textless });

      requested = [];
      const external = await parser.parse(pdf, { table: { mode: 'external_only' }, onTableStructure: async request => {
        check(request.image.blob instanceof Blob && request.image.blob.type === 'image/png' && request.image.blob.size > 0, 'Crop must be an owned PNG');
        check(request.image.width > 0 && request.image.height > 0, 'Crop dimensions missing');
        check(Math.abs(request.crop_to_viewport.e - request.crop_bbox.left) < 1e-6 && Math.abs(request.crop_to_viewport.f - request.crop_bbox.top) < 1e-6, 'Crop transform origin mismatch');
        requested.push(request.request_id); return supplied(request);
      } });
      check(requested.length === count && new Set(requested).size === count, 'External-only requests are missing or duplicated');
      check(tables(external).every(b => hasText(b) ? b.table?.source === 'external_tsr' && b.table.cells.length === 1 && !b.table.cells[0].is_header : !b.table), 'Local rules overwrote declared external topology or source availability');
      check(textless === 0 || external.pages.some(p => p.warnings.some(w => w.code === 'TableTextAssignmentFailed')), 'Textless regions must report unavailable source text');
      check(facts(external) === baselineFacts, 'External input changed canonical source facts');
      cases.push({ name: 'external_only', requested: requested.length, completed: count - textless });

      const invalid = await parser.parse(pdf, { table: { mode: 'external_only' }, onTableStructure: async request => ({ ...supplied(request), request_id: 'wrong-request' }) });
      check(tables(invalid).every(b => !b.table), 'Invalid response silently used local topology');
      check(invalid.pages.some(p => p.warnings.some(w => w.code === 'InvalidTsrInput')), 'Invalid input diagnostic missing');
      check(facts(invalid) === baselineFacts, 'Invalid external input changed source text');
      cases.push({ name: 'invalid_response' });

      let ordinal = 0;
      const accepted = new Set();
      const partial = await parser.parse(pdf, { table: { mode: 'external_only' }, onTableStructure: async request => {
        if (ordinal++ % 2 === 0) throw new Error('One caller-owned table failed');
        accepted.add(request.block_id);
        return supplied(request);
      } });
      check(tables(partial).every(b => Boolean(b.table) === (accepted.has(b.id) && hasText(b))), 'A failed table affected another table result');
      check(partial.pages.some(p => p.blocks.some(b => b.table) && p.blocks.some(b => b.label === 'table' && !b.table)), 'Input must exercise partial success within one page');
      check(facts(partial) === baselineFacts, 'Partial success changed canonical source facts');
      cases.push({ name: 'same_page_partial_success', completed: tables(partial).filter(b => b.table).length, tables: count });

      const signals = [];
      const timed = await parser.parse(pdf, { table: { mode: 'external_only', timeout_ms: 500 }, onTableStructure: (_request, signal) => {
        signals.push(signal);
        return new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(new Error('provider canceled')), { once: true }));
      } });
      check(signals.length > 0 && signals.every(s => s.aborted), 'Deadline did not cancel caller-owned provider work');
      check(tables(timed).every(b => !b.table) && timed.pages.some(p => p.warnings.some(w => w.code === 'TableExternalTimeout')), 'Deadline did not preserve an unresolved source table');
      check(facts(timed) === baselineFacts, 'Timeout changed source text');
      cases.push({ name: 'timeout', canceled: signals.length });

      const controller = new AbortController();
      let received, late, tableSignal;
      const entered = new Promise(resolve => { received = resolve; });
      const active = parser.parse(pdf, { signal: controller.signal, table: { mode: 'external_only' }, onTableStructure: (request, signal) => {
        tableSignal = signal; received(); return new Promise(resolve => { late = () => resolve(supplied(request)); });
      } });
      const settled = active.then(() => ({ code: 'unexpected-success' }), error => ({ code: error.code }));
      await entered; controller.abort(); const aborted = await settled;
      check(aborted.code === 'Aborted' && tableSignal.aborted, 'Parse cancellation did not abort the table callback');
      late(); await Promise.resolve(); await Promise.resolve();
      cases.push({ name: 'abort_and_late_reply' });
      return { status: 'passed', kind: 'supplied-input SDK contract', modelAccuracyEvaluated: false, cases };
    } finally { await parser.close(); }
  }, { bytes, baseUrl });
  assert.equal(report.status, 'passed');
  return report;
}

// Run from the repository root after building and serving packages/web; reuse its installed Playwright.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.argv[2]) throw new Error('Usage: node crates/web/tests/table_input.mjs <table-containing.pdf> [base-url]');
  const { chromium } = await import('../../../packages/web/node_modules/playwright/index.mjs');
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  try {
    const page = await browser.newPage();
    page.on('pageerror', error => console.error(error));
    console.log(JSON.stringify(await runTableInputChecks(page, { pdfPath: process.argv[2], baseUrl: process.argv[3] }), null, 2));
  } finally { await browser.close(); }
}
