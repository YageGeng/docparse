import assert from 'node:assert/strict';
import { tableCoverage } from './coverage.mjs';
import { mkdir, writeFile } from 'node:fs/promises';
import { basename, dirname, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { chromium } from '../../../packages/web/node_modules/playwright/index.mjs';

const { values, positionals: files } = parseArgs({ allowPositionals: true, options: {
  mode: { type: 'string', default: 'fallback' },
  output: { type: 'string', default: 'packages/web/test-results/paddle-tsr/browser-default.json' },
  base: { type: 'string', default: 'http://127.0.0.1:8768' },
  'allow-unresolved': { type: 'boolean', default: false },
} });
assert(files.length > 0, 'Supply actual PDF paths after the options');
assert(['external_only','fallback','rules_only'].includes(values.mode));
const output = resolve(values.output);
await mkdir(dirname(output), { recursive: true });
const report = { status: 'running', model: 'PaddlePaddle/SLANet_plus_onnx', revision: '7dbe640e127602bf506815e822c09758de73c482', mode: values.mode, startedAt: new Date().toISOString(), runs: [] };
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage({ viewport: { width: 1680, height: 1100 } });
await page.addInitScript(() => {
  const NativeWorker = Worker;
  window.lastDocument = undefined; window.parserTimings = [];
  // Observe the production Worker without changing inference inputs or outputs.
  window.Worker = class extends NativeWorker {
    /** Records production parse results and timings without substituting the backend. */
    constructor(...args) {
      super(...args);
      this.addEventListener('message', ({ data }) => {
        if (data.event === 'timing') window.parserTimings.push(data.value);
        if (data.ok && data.method === 'parse') window.lastDocument = data.value;
      });
    }
  };
});
const progress = setInterval(async () => console.log(await page.locator('#status-detail').textContent().catch(() => 'loading')), 15000);
try {
  await page.goto(`${values.base}/example/`);
  assert.equal(await page.locator('#table-mode').inputValue(), 'fallback', 'The production UI must default to rules-first TSR');
  if (values.mode !== 'fallback') await page.locator('#table-mode').selectOption(values.mode);
  for (const [index, file] of files.entries()) {
    const started = Date.now();
    await page.evaluate(() => { window.parserTimings = []; window.lastDocument = undefined; });
    await page.locator('#file').setInputFiles(file);
    await page.locator('#parse').click();
    await page.waitForFunction(() => document.querySelector('#stage').textContent === 'Your document is ready' || document.querySelector('.status-card').dataset.state === 'error', null, { timeout: 300000 });
    assert.equal(await page.locator('#stage').textContent(), 'Your document is ready', await page.locator('#status-detail').textContent());
    const result = await page.evaluate(() => ({
      errors: window.lastDocument.errors,
      timings: window.parserTimings,
      pages: window.lastDocument.pages.map(p => ({ page: p.page_number, warnings: p.warnings, tables: p.blocks.filter(b => b.label === 'table').map(b => ({ id: b.id, bbox: b.bbox, hasText: b.lines.some(l => l.text_items.some(i => i.raw_text.trim())), table: b.table ?? null, evidence: b.evidence })) })),
    }));
    await writeFile(resolve(dirname(output), `${index}-${values.mode}.json`), JSON.stringify(result));
    assert.equal(result.errors.length, 0);
    const tables = result.pages.flatMap(p => p.tables);
    const structured = tables.filter(t => t.table);
    const models = structured.filter(t => t.table.source === 'external_tsr');
    const inference = result.timings.filter(t => t.stage === 'tsr_inference');
    const coverage = tableCoverage(tables.length, structured.length, values['allow-unresolved']);
    if (values.mode === 'external_only') {
      assert.equal(models.length, structured.length, 'Default TSR silently substituted local rules');
      assert(inference.length >= models.length && inference.length > 0, 'No real table model inference reached the output');
      assert(models.every(t => t.evidence.some(e => e.kind === 'external_table_structure' && e.details.engine === 'slanet-plus-onnx-webgpu')), 'Structured output must identify the real model');
    }
    if (values.mode === 'external_only' && !values['allow-unresolved'] && basename(file) === '2303.18223v16.pdf') {
      const at = number => result.pages.find(p => p.page === number).tables;
      const shapes = [[8,58,13],[24,16,3],[33,6,13],[47,28,3],[57,21,4],[68,19,6],[82,11,2]];
      for (const [number,rows,columns] of shapes) {
        const table = at(number)[0].table;
        assert.deepEqual([table.row_count,table.column_count],[rows,columns],`Page ${number} lost source rows or columns`);
      }
      const cells = at(8)[0].table.cells;
      const t5 = cells.find(c => c.text === 'T5 [82]');
      const mt5 = cells.find(c => c.text === 'mT5 [83]');
      assert(t5 && mt5 && mt5.row === t5.row + 1 && t5.column === 1 && mt5.column === 1, 'Page 8 model names shifted rows');
      assert(cells.some(c => c.row === t5.row && c.column === 2 && c.text === 'Oct-2019'));
      assert(cells.some(c => c.row === mt5.row && c.column === 2 && c.text === 'Oct-2020'));
      assert(at(33)[0].table.cells.some(c => c.row === 2 && c.column === 8 && c.text === '36.6'));
      assert(at(57)[0].table.cells.some(c => c.row === 2 && c.column === 3 && c.text.includes('WMT')));
    }
    if (values.mode === 'fallback' && basename(file) === 'Terminal-Universe_ Turning Agent Trajectories into Scalable Terminal Environments.pdf') {
      const [statistics, configurations] = result.pages.find(p => p.page === 32).tables;
      assert.deepEqual([statistics.table?.row_count, statistics.table?.column_count], [5, 5]);
      for (const [column, text] of ['Dataset', 'Records', 'Turns', 'Tool calls', 'Tokens'].entries()) {
        const cell = statistics.table.cells.find(c => c.row === 0 && c.column === column);
        assert(cell && cell.column_span === 1 && cell.text.trim() === text, `Statistics header ${text} was merged`);
      }
      const table = configurations.table;
      assert.equal(table?.source, 'external_tsr', 'The unresolved configuration table must reach the real TSR');
      assert.deepEqual([table.row_count, table.column_count], [16, 7]);
      assert(table.cells.some(c => c.row === 11 && c.column === 0 && c.column_span === 7 && c.text === 'Shared across all reproductions'));
      for (const [row, value] of [[12,'4 h'],[13,'10 h'],[14,'6'],[15,'4']]) {
        assert(table.cells.some(c => c.row === row && c.column === 1 && c.column_span === 6 && c.text === value), `Shared setting ${value} lost its span`);
      }
      assert(table.cells.every(c => c.is_header === [0,11].includes(c.row)));
    }
    if (values.mode === 'rules_only') assert.equal(inference.length, 0);
    if (values.mode === 'fallback') assert(inference.length >= tables.length - (structured.length - models.length), 'Every unresolved local table must reach the real model, including segmented retries');
    for (const block of structured) for (const cell of block.table.cells) {
      if (!cell.bbox) { assert.equal(cell.lines.length, 0, 'Empty cells may omit measured ink geometry'); continue; }
      assert(cell.bbox.left >= block.bbox.left - 1e-6 && cell.bbox.right <= block.bbox.right + 1e-6 && cell.bbox.top >= block.bbox.top - 1e-6 && cell.bbox.bottom <= block.bbox.bottom + 1e-6, 'Cell escaped its layout boundary');
    }
    const record = { file: basename(file), status: coverage, pages: result.pages.length, tableCount: tables.length, textTables: tables.filter(t => t.hasText).length, structured: structured.length, modelTables: models.length, modelCalls: inference.length, tableRequests: result.timings.filter(t => t.stage === 'table_external').length, executionProvider: await page.locator('#engine-status').getAttribute('data-provider'), elapsedMs: Date.now() - started };
    await writeFile(resolve(dirname(output), `${index}-${values.mode}.json`), JSON.stringify(result));
    report.runs.push(record); console.log(JSON.stringify(record));
    // Validate the inspector against a real model result, including merged HTML cells.
    const terminal = basename(file).startsWith('Terminal-Universe_');
    const first = terminal ? result.pages.find(p => p.page === 32) : result.pages.find(p => p.tables.some(t => t.table));
    if (first) {
      const table = terminal ? first.tables.at(-1) : first.tables.find(t => t.table);
      await page.getByRole('button', { name: `Show page ${first.page}`, exact: true }).click();
      await page.evaluate(async () => {
        const image = new Image(); image.src = document.querySelector('.page-sheet image').getAttribute('href');
        await image.decode();
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      });
      // Inspect both local and model-backed tables on the reported mixed-source page.
      for (const selected of (terminal ? first.tables : [table]).filter(b => b.table)) {
        await page.locator('#block-select').selectOption(selected.id);
        const label = selected.table.source === 'external_tsr' ? 'TSR input' : 'Rules';
        assert((await page.locator('#selection-meta').textContent()).includes(`Source: ${label}`), 'Selected table source is missing or stale');
        assert.equal(await page.locator('.table-view').getAttribute('data-source'), selected.table.source);
        if (terminal) await page.screenshot({ path: resolve(dirname(output), `${index}-${values.mode}-${selected.table.source}.png`) });
      }
      await page.locator('#block-select').selectOption(table.id);
      assert.equal(await page.locator('.table-view tr').count(), table.table.row_count);
      await page.screenshot({ path: resolve(dirname(output), `${index}-${values.mode}.png`) });
    }
    await writeFile(output, JSON.stringify(report, null, 2));
  }
  report.status = report.runs.every(run => run.status === 'passed') ? 'passed' : 'partial';
} catch (error) {
  report.status = 'failed'; report.error = error.stack; throw error;
} finally {
  clearInterval(progress);
  report.finishedAt = new Date().toISOString();
  await writeFile(output, JSON.stringify(report, null, 2));
  await browser.close();
}
