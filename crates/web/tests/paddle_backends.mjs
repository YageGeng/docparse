import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { chromium } from '../../../packages/web/node_modules/playwright/index.mjs';

// Use a real single-page table PDF so GPU submissions during TSR cannot include another page's layout run.
const bytes = [...await readFile(process.argv[2])];
const output = resolve(process.argv[3] ?? 'packages/web/test-results/paddle-tsr/backends.json');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const recorder = await readFile(new URL('../../../packages/web/tests/instrumented-worker.js', import.meta.url), 'utf8');
await page.route('**/__tsr-observe.js?*', route => route.fulfill({ contentType: 'text/javascript', body: recorder }));
await page.addInitScript(() => {
  const NativeWorker = Worker;
  window.Worker = class extends NativeWorker {
    /** Records real sessions; fault injection affects capability/initialization only. */
    constructor(url, options) {
      const entry = new URL('/__tsr-observe.js', location.href);
      entry.searchParams.set('worker', String(url));
      if (window.backendFault) entry.searchParams.set(window.backendFault, '1');
      super(entry, options);
      this.addEventListener('message', ({ data }) => { if (data.metrics) window.backendMetrics = data.metrics; });
    }
  };
});
const report = { status: 'running', startedAt: new Date().toISOString(), runs: [] };
try {
  await page.goto('http://127.0.0.1:8768/example/');
  // Backend selection belongs to the Worker API; serialized defaults contain no provider fields.
  const defaults = await page.evaluate(async () => {
    const { default: initialize, default_config } = await import('/dist/pkg/docparse_web.js');
    await initialize();
    const raw = default_config();
    return { hasProvider: [raw.layout, raw.tsr, raw.ocr].some(group => 'execution_provider' in group), mode: raw.tsr.mode };
  });
  assert.deepEqual(defaults, { hasProvider: false, mode: 'fallback' });
  report.rustDefaults = defaults;
  for (const scenario of [
    { name: 'default-webgpu', expected: 'webgpu' },
    { name: 'explicit-cpu', provider: 'wasm', expected: 'wasm' },
    { name: 'missing-gpu-rejected', fault: 'noGpu', error: 'ExecutionProviderUnavailable' },
    { name: 'missing-gpu-fallback', fault: 'noGpu', fallback: true, expected: 'wasm' },
    { name: 'tsr-gpu-init-rejected', fault: 'failTsrGpuInit', error: 'ExecutionProviderInitializationFailed' },
    { name: 'tsr-gpu-init-fallback', fault: 'failTsrGpuInit', fallback: true, expected: 'wasm' },
  ]) {
    const result = await page.evaluate(async ({ scenario, bytes }) => {
      window.backendFault = scenario.fault;
      window.backendMetrics = undefined;
      const { createParser } = await import('/dist/index.js');
      let parser;
      try {
        parser = await createParser({
          ...(scenario.provider ? { executionProvider: scenario.provider } : {}),
          allowCpuFallback: scenario.fallback ?? false,
          // Backend checks force both models; ordinary parsing defaults to rules first.
          config: { tsr: { mode: 'external_only' } },
          artifacts: { kind: 'urls', model: '/models/inference.onnx', config: '/models/inference.yml', manifest: '/models/model-manifest.json' },
          tsrArtifacts: { kind: 'urls', model: '/models/slanet-plus/inference.onnx', config: '/models/slanet-plus/inference.yml', manifest: '/models/slanet-plus/model-manifest.json' },
        });
        const document = await parser.parse(new Uint8Array(bytes));
        return { provider: parser.executionProvider, pages: document.pages.length, errors: document.errors,
          tables: document.pages.flatMap(p => p.blocks.filter(b => b.label === 'table').map(b => ({ table: b.table, evidence: b.evidence }))), metrics: window.backendMetrics };
      } catch (error) { return { error: error.code, message: error.message, metrics: window.backendMetrics }; }
      finally { await parser?.close(); }
    }, { scenario, bytes });
    report.runs.push({ scenario, ...result });
    await mkdir(dirname(output), { recursive: true });
    await writeFile(output, JSON.stringify(report, null, 2));
    if (scenario.error) assert.equal(result.error, scenario.error, JSON.stringify(result));
    else {
      assert.equal(result.error, undefined, result.message);
      assert.equal(result.provider, scenario.expected);
      assert.equal(result.pages, 1, 'Supply one real table page for unambiguous per-model GPU measurements');
      assert.equal(result.errors.length, 0);
      assert(result.tables.length > 0);
      assert.equal(result.metrics.sessions, 2, 'A failed GPU attempt leaked a live session');
      assert.equal(result.metrics.liveTensors, 0, 'Inference retained completed input/output tensors');
      const engine = `slanet-plus-onnx-${scenario.expected === 'webgpu' ? 'webgpu' : 'cpu'}`;
      assert(result.tables.every(b => b.table?.source === 'external_tsr' && b.evidence.some(e => e.details?.engine === engine)));
      for (const name of ['layout', 'tsr']) {
        const model = result.metrics.models.find(m => m.name === name && m.calls > 0);
        assert(model && model.providers.some(p => (p.name ?? p) === scenario.expected), `${name} did not use ${scenario.expected}`);
        assert(scenario.expected === 'webgpu' ? model.gpuSubmissions > 0 : model.gpuSubmissions === 0, `${name} GPU activity contradicts provider`);
      }
    }
    console.log(JSON.stringify({ scenario: scenario.name, provider: result.provider, error: result.error, models: result.metrics?.models }));
  }
  report.status = 'passed';
} catch (error) { report.status = 'failed'; report.error = error.stack; throw error; }
finally {
  report.finishedAt = new Date().toISOString();
  await mkdir(dirname(output), { recursive: true });
  await writeFile(output, JSON.stringify(report, null, 2));
  await browser.close();
}
