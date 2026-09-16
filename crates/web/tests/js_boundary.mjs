import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { chromium } from '../../../packages/wasm-web/node_modules/playwright/index.mjs';

const output = resolve(process.argv[3] ?? 'packages/wasm-web/test-results/js-boundary/report.json');
const bytes = [...await readFile(process.argv[2])];
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const recorder = await readFile(new URL('../../../packages/wasm-web/tests/instrumented-worker.js', import.meta.url), 'utf8');
// Inject only JS property behavior. Successful parsing still uses the production Worker and real models.
let script = recorder.replace('runtime = value;', `runtime = value;
    const fault = new URL(location.href).searchParams.get('fault');
    if (fault === 'readonly') Object.defineProperty(value.env.wasm, 'proxy', { value: false, writable: false });
    if (fault === 'getter') Object.defineProperty(value.env, 'wasm', { get() { throw new Error('runtime getter rejected'); } });`);
// The production ABI receives actual JS functions here, unlike the main-thread SDK message channel.
script = script.replace('await import(new URL(location.href).searchParams.get("worker"));', `
const production = new URL(location.href).searchParams.get("worker");
const { WebParser } = await import(new URL("./pkg/docparse_web.js", production).href);
const parse = WebParser.prototype.parse_with_options;
/** Perturbs caller-owned callbacks while retaining the real parser and layout model. */
WebParser.prototype.parse_with_options = function(bytes, options, callbacks) {
  const fault = new URL(location.href).searchParams.get('fault');
  if (fault === 'callback-getter') Object.defineProperty(callbacks, 'progress', { get() { throw new Error('callback getter rejected'); } });
  if (fault === 'callback-type') callbacks.progress = 42;
  if (fault === 'callback-throw') callbacks.progress = () => { throw new Error('observer rejected'); };
  if (fault.startsWith('table-')) {
    options = { mode: 'tsr_only' };
    callbacks.table_cancel = () => {};
    callbacks.table_request = request => {
      if (fault === 'table-throw') throw Object.defineProperty({}, 'message', { get() { throw new Error('nested message getter rejected'); } });
      if (fault === 'table-reject') return Promise.reject('provider rejected');
      if (!(request.pixels instanceof Uint8Array) || request.pixels.length !== request.width * request.height * 3 || request.pixels.buffer === rustMemory.buffer) throw new Error('crop pixels are not an independent RGB copy');
      const value = { request_id: request.request_id, structure_tokens: ['<tr>', '<td></td>', '</tr>'], cell_bboxes: [[0, 0, request.width, request.height]] };
      return fault === 'table-thenable' ? { then(resolve) { resolve(value); } } : value;
    };
  }
  return parse.call(this, bytes, options, callbacks);
};
await import(production);`);
await page.route('**/__js-boundary.js?*', route => route.fulfill({ contentType: 'text/javascript', body: script }));
await page.addInitScript(() => {
  const NativeWorker = Worker;
  window.Worker = class extends NativeWorker {
    /** Records and perturbs JS boundary behavior without replacing inference. */
    constructor(url, options) {
      const entry = new URL('/__js-boundary.js', location.href);
      entry.searchParams.set('worker', String(url));
      entry.searchParams.set('fault', window.jsFault ?? '');
      super(entry, options);
    }
  };
});
const report = { status: 'running', cases: [] };
try {
  await page.goto('http://127.0.0.1:8768/example/');
  for (const fault of ['readonly', 'getter', 'callback-getter', 'callback-type', 'callback-throw', 'table-throw', 'table-reject', 'table-value', 'table-thenable', 'none']) {
    const result = await page.evaluate(async ({ fault, bytes }) => {
      window.jsFault = fault;
      const { createParser } = await import('/dist/index.js');
      let parser;
      try {
        const progress = [], images = [], timings = [];
        parser = await createParser({
          config: { formula: { inline_enabled: false, display_enabled: false }, tsr: { mode: 'rules_only' } },
          artifacts: { kind: 'urls', model: '/models/inference.onnx', config: '/models/inference.yml', manifest: '/models/model-manifest.json' },
        });
        const document = await parser.parse(new Uint8Array(bytes), {
          onProgress: event => progress.push(event), onPageImage: image => images.push(image), onTiming: event => timings.push(event),
        });
        return { warnings: document.pages.flatMap(p => p.warnings), tables: document.pages.flatMap(p => p.blocks.filter(b => b.label === 'table').map(b => b.table?.source ?? null)), pages: document.pages.length, errors: document.errors, progress: progress.length, timings: timings.length, images: images.map(i => ({ width: i.width, height: i.height, bytes: i.blob.size })) };
      } catch (error) { return { code: error.code, message: error.message }; }
      finally { await parser?.close(); }
    }, { fault, bytes });
    report.cases.push({ fault, ...result });
    if (['readonly', 'getter'].includes(fault)) assert.equal(result.code, 'RuntimeInitializationFailed', JSON.stringify(result));
    else if (['callback-getter', 'callback-type'].includes(fault)) assert.equal(result.code, 'InvalidOptions', JSON.stringify(result));
    else {
      assert.equal(result.code, undefined, result.message);
      assert.equal(result.errors.length, 0);
      assert(result.pages > 0 && result.timings > 0);
      assert(fault === 'callback-throw' ? result.progress === 0 : result.progress > 0);
      if (['table-throw', 'table-reject'].includes(fault)) {
        assert(result.tables.length > 0 && result.tables.every(t => t === null));
        const message = fault === 'table-throw' ? 'JavaScript operation failed' : 'provider rejected';
        assert(result.warnings.some(w => w.message.includes(message)), JSON.stringify(result.warnings));
      }
      if (['table-value', 'table-thenable'].includes(fault)) assert(result.tables.length > 0 && result.tables.every(t => t === 'external_tsr'));
      assert.equal(result.images.length, result.pages);
      assert(result.images.every(i => i.width > 0 && i.height > 0 && i.bytes > 0));
    }
    console.log(JSON.stringify({ fault, ...result }));
  }
  report.status = 'passed';
} catch (error) { report.status = 'failed'; report.error = error.stack; throw error; }
finally {
  await mkdir(dirname(output), { recursive: true });
  await writeFile(output, JSON.stringify(report, null, 2));
  await browser.close();
}
