import { runExampleE2E } from "./example.e2e.mjs";
import { runExportRaceE2E } from "./export-race.e2e.mjs";

/** One suite shared by the command-line browser and Codex Browser Use; no parsing backend is replaced. */
export async function* runE2E({ page, navigate, cdp }, environment, { cycles = 3, provider = "wasm" } = {}) {
  const sdk = new URL("/packages/web/tests/browser.html", environment.sdkOrigin);
  sdk.searchParams.set("cycles", String(cycles)); sdk.searchParams.set("run", "organized-e2e");
  sdk.searchParams.set("provider", provider);
  sdk.searchParams.set("growMemory", "1");
  await navigate(sdk.href);
  const deadline = Date.now() + 600000;
  while (true) {
    let report;
    const text = await page.locator("#results").textContent();
    try { report = JSON.parse(text); } catch { /* The initial page displays a plain Starting message. */ }
    if (report?.status === "failed") throw new Error(report.error ?? JSON.stringify(report.tests));
    if (report?.status === "passed") {
      yield { suite: "sdk", kind: "check", name: "Real-model SDK, parity, progress, and lifecycle acceptance", report };
      break;
    }
    if (Date.now() >= deadline) throw new Error("SDK acceptance did not complete before its deadline");
    yield { suite: "sdk", kind: "progress", stage: report?.tests?.at(-1) ?? "Initializing real SDK" };
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  await navigate(environment.exampleUrl);
  for await (const event of runExampleE2E(page, { ...environment, expectedProvider: provider === "webgpu" ? "webgpu" : undefined })) yield { suite: "ui", ...event };
  for await (const event of runExportRaceE2E(page, cdp)) yield { suite: "export", kind: "check", ...event };
}
