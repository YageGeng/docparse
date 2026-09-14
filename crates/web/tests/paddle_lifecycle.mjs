import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { chromium } from '../../../packages/wasm-web/node_modules/playwright/index.mjs';

const bytes = [...await readFile(process.argv[2])];
const output = process.argv[3] ?? 'packages/wasm-web/test-results/paddle-tsr/lifecycle.json';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
let canceled = false;
const logs = [];
page.on('console', message => {
  if (message.type() === 'warning' || message.type() === 'error') logs.push(message.text());
  if (!canceled && message.text().includes('requesting table structure') && message.text().includes('slanet-plus-onnx-webgpu')) {
    canceled = true;
    void page.evaluate(() => window.abortTsr()).catch(() => {});
  }
});
try {
  await page.goto('http://127.0.0.1:8768/example/');
  const report = await page.evaluate(async bytes => {
    const { createParser } = await import('/dist/index.js');
    const options = {
      // Omitted provider must select WebGPU; explicitly request model work for lifecycle checks.
      config: { tsr: { mode: 'tsr_only' } },
      artifacts: { kind: 'urls', model: '/models/inference.onnx', config: '/models/inference.yml', manifest: '/models/model-manifest.json' },
      tsrArtifacts: { kind: 'urls', model: '/models/slanet-plus/inference.onnx', config: '/models/slanet-plus/inference.yml', manifest: '/models/slanet-plus/model-manifest.json' },
      tsrCellArtifacts: { kind: 'urls', model: '/models/rtdetr-table-cell-wireless/inference.onnx', config: '/models/rtdetr-table-cell-wireless/inference.yml', manifest: '/models/rtdetr-table-cell-wireless/model-manifest.json' },
    };
    const data = new Uint8Array(bytes);
    const controller = new AbortController();
    window.abortTsr = () => controller.abort();
    let parser = await createParser(options);
    let abortCode;
    try { await parser.parse(data, { signal: controller.signal }); }
    catch (error) { abortCode = error.code; }
    await parser.close();
    if (abortCode !== 'Aborted') throw new Error(`Expected actual model cancellation, got ${abortCode}`);
    parser = await createParser(options);
    /** Compares the model's table semantics across repeated actual inference calls. */
    const tables = doc => doc.pages.flatMap(p => p.blocks.filter(b => b.label === 'table').map(b => b.table ? [b.table.source, b.table.row_count, b.table.column_count, b.table.cells.map(c => [c.row,c.column,c.row_span,c.column_span,c.text])] : null));
    try {
      const first = await parser.parse(data);
      const timed = await parser.parse(data, { table: { mode: 'tsr_only', timeout_ms: 1 } });
      const repeated = await parser.parse(data);
      const codes = timed.pages.flatMap(p => p.warnings.map(w => w.code));
      return { abortCode, recoveredAfterAbort: tables(first).some(Boolean), stableReuse: JSON.stringify(tables(first)) === JSON.stringify(tables(repeated)), deadlineObserved: codes.includes('TableExternalTimeout'), timedCodes: codes, repeatedErrors: repeated.errors, repeatedWarnings: repeated.pages.map(p => p.warnings), firstTables: tables(first), repeatedTables: tables(repeated), provider: parser.executionProvider };
    } finally { await parser.close(); }
  }, bytes);
  report.logs = logs;
  await writeFile(output, JSON.stringify(report, null, 2));
  assert.equal(report.abortCode, 'Aborted');
  assert.equal(report.repeatedErrors.length, 0);
  assert(!report.repeatedWarnings.flat().some(w => w.code === 'LayoutUnavailable'), 'Timed-out TSR overlapped the next layout readback');
  assert(report.recoveredAfterAbort && report.stableReuse);
  assert(report.deadlineObserved, 'A real TSR run exceeded its deadline without reporting timeout');
  console.log(JSON.stringify({ ...report, firstTables: undefined, repeatedTables: undefined, logs: undefined }));
} finally { await browser.close(); }
