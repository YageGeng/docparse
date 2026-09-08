import { spawn, spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import process from "node:process";
import { runE2E } from "./suite.e2e.mjs";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(packageRoot, "../..");

/** Runs deterministic build/reference preparation before creating a browser. */
function run(command, args, env = process.env) {
  const result = spawnSync(command, args, { cwd: root, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed with status ${result.status}`);
}

/** Starts one owned server on an ephemeral port and reads its explicit ready announcement. */
async function startServer(command, args, env, children) {
  const child = spawn(command, args, { cwd: root, env, stdio: ["ignore", "pipe", "pipe"] });
  children.push(child);
  return await new Promise((resolve, reject) => {
    let output = "", errors = "";
    const timer = setTimeout(() => { child.kill(); reject(new Error(`Server startup timed out: ${errors}`)); }, 15000);
    child.stderr.on("data", data => { errors = (errors + data).slice(-8192); });
    child.stdout.on("data", data => {
      output = (output + data).slice(-8192);
      const url = output.match(/http:\/\/127\.0\.0\.1:\d+/)?.[0];
      if (url) { clearTimeout(timer); resolve(url); }
    });
    child.once("error", error => { clearTimeout(timer); reject(error); });
    child.once("exit", code => { clearTimeout(timer); reject(new Error(`Server exited with ${code}: ${errors}`)); });
  });
}

/** Builds production artifacts and native references, then owns both test server lifetimes. */
export async function prepareE2E() {
  run(process.execPath, [join(packageRoot, "scripts/build.mjs")]);
  run(process.execPath, [join(packageRoot, "node_modules/typescript/bin/tsc"), "-p", join(packageRoot, "example/tsconfig.json")]);
  run("cargo", ["test", "-p", "docparse-core", "--locked", "--test", "web_reference", "--", "--ignored", "--nocapture"], {
    ...process.env, DOCPARSE_WEB_REFERENCE_DIR: join(packageRoot, "test-results/native"),
  });
  const reportDirectory = join(packageRoot, "test-results/e2e");
  await mkdir(reportDirectory, { recursive: true });
  const invalidPdf = join(reportDirectory, "invalid.pdf");
  await writeFile(invalidPdf, "%PDF-1.7\nInvalid E2E input\n");
  const children = [];
  /** Stops only servers created by this run, including partially completed setup. */
  const close = async () => {
    await Promise.all(children.map(child => new Promise(resolve => {
      if (!child.pid || child.exitCode !== null || child.signalCode !== null) { resolve(); return; }
      child.once("exit", resolve); child.kill();
    })));
  };
  try {
    const exampleOrigin = await startServer(process.execPath, [join(packageRoot, "scripts/serve-example.mjs")], { ...process.env, PORT: "0" }, children);
    const sdkOrigin = await startServer("python3", [join(packageRoot, "tests/serve.py"), "--port", "0"], process.env, children);
    const pdf = join(root, "crates/core/tests/fixtures/pdf/multipage_layout.pdf");
    return { exampleUrl: `${exampleOrigin}/example/`, sdkOrigin, pdf, invalidPdf, cancellationPdf: pdf, reportDirectory, close };
  } catch (error) { await close(); throw error; }
}

/** Provides a complete CLI run with reports, screenshots and owned browser/server cleanup. */
async function main() {
  const { values } = parseArgs({ options: { help: { type: "boolean" }, headed: { type: "boolean" }, channel: { type: "string" }, cycles: { type: "string", default: "3" }, provider: { type: "string", default: "wasm" } } });
  if (values.help) {
    console.log("Usage: npm run test:e2e -- [--headed] [--channel chrome] [--cycles 20] [--provider wasm|webgpu]\nBuilds SDK/example, generates native references, then runs SDK, UI and export E2E against the real model.\nInstall the pinned model and Chromium first: npx playwright install chromium\nReports: packages/web/test-results/e2e/latest.json");
    return;
  }
  const cycles = Number(values.cycles);
  if (!Number.isSafeInteger(cycles) || cycles < 1) throw new Error("--cycles must be a positive integer");
  if (!["wasm", "webgpu"].includes(values.provider)) throw new Error("--provider must be wasm or webgpu");
  const environment = await prepareE2E();
  const report = { status: "running", startedAt: new Date().toISOString(), events: [] };
  let browser, page;
  let interrupted = false;
  // Closing the browser interrupts an active locator wait; normal failure cleanup
  // still writes the report and stops the servers instead of leaving orphan jobs.
  const interrupt = () => { interrupted = true; void browser?.close(); };
  process.once("SIGINT", interrupt); process.once("SIGTERM", interrupt);
  try {
    const { chromium } = await import("playwright");
    browser = await chromium.launch({ headless: !values.headed, channel: values.channel });
    if (interrupted) throw new Error("E2E run interrupted");
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
    page = await context.newPage();
    const cdp = await context.newCDPSession(page);
    let lastProgress = "";
    for await (const event of runE2E({ page, cdp, navigate: url => page.goto(url) }, environment, { cycles, provider: values.provider })) {
      if (interrupted) throw new Error("E2E run interrupted");
      report.events.push(event);
      const progress = `${event.suite}: ${event.name ?? event.mode ?? event.stage}`;
      if (event.kind === "check" || progress !== lastProgress) console.log(progress);
      lastProgress = progress;
    }
    report.status = "passed";
  } catch (error) {
    report.status = "failed"; report.error = error.stack ?? String(error); throw error;
  } finally {
    process.removeListener("SIGINT", interrupt); process.removeListener("SIGTERM", interrupt);
    report.finishedAt = new Date().toISOString();
    try {
      if (page) await page.screenshot({ path: join(environment.reportDirectory, "last-page.png") }).catch(() => {});
      await writeFile(join(environment.reportDirectory, "latest.json"), JSON.stringify(report, null, 2) + "\n");
    } finally { try { await browser?.close(); } finally { await environment.close(); } }
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error); process.exitCode = 1; });
}
