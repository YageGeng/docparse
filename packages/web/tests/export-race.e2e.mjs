import assert from "node:assert/strict";

/** Evaluates a local timing probe while surfacing browser-side exceptions to the test. */
async function inspect(cdp, expression) {
  const response = await cdp.send("Runtime.evaluate", { expression, returnByValue: true });
  assert(!response.exceptionDetails, JSON.stringify(response.exceptionDetails));
  return response.result.value;
}

/** Delays only real PNG callbacks; PDFium, model inference and PNG encoding remain unchanged.
 * Run against an isolated example tab after parsing the three-page fixture. Both Browser Use
 * and Playwright CDP sessions provide the send method consumed by this test.
 */
export async function* runExportRaceE2E(page, cdp) {
  await inspect(cdp, `(() => {
    const original = HTMLCanvasElement.prototype.toBlob;
    const create = URL.createObjectURL, revoke = URL.revokeObjectURL;
    const state = window.__docparseExportTiming = { original, create, revoke, pending: [], urls: new Set() };
    HTMLCanvasElement.prototype.toBlob = function(callback, ...options) {
      const item = { callback, blob: null, ready: false }; state.pending.push(item);
      original.call(this, blob => { item.blob = blob; item.ready = true; }, ...options);
    };
    URL.createObjectURL = function(blob) { const url = create.call(URL, blob); state.urls.add(url); return url; };
    URL.revokeObjectURL = function(url) { state.urls.delete(url); return revoke.call(URL, url); };
    return true;
  })()`);
  try {
    for (const mode of ["newer-first", "older-first", "older-failure"]) {
      const offset = await inspect(cdp, "window.__docparseExportTiming.pending.length");
      await page.getByRole("button", { name: "Show page 1", exact: true }).click();
      await page.locator("#download").click();
      await page.locator("#export-dialog").waitFor({ state: "visible" });
      if (mode === "older-first") await page.locator("#export-dialog").press("Escape");
      else await page.locator("#close-export").click();
      assert.equal(await page.locator("#export-dialog").isVisible(), false);
      await page.getByRole("button", { name: "Show page 2", exact: true }).click();
      await page.locator("#download").click();
      await page.locator("#export-dialog").waitFor({ state: "visible" });
      let encoded = false;
      for (let attempt = 0; attempt < 50; attempt++) {
        encoded = await inspect(cdp, `window.__docparseExportTiming.pending.slice(${offset}).length === 2 && window.__docparseExportTiming.pending.slice(${offset}).every(item => item.ready && item.blob)`);
        if (encoded) break;
        await new Promise(resolve => setTimeout(resolve, 100));
      }
      assert(encoded, "Real PNG encoding did not complete");
      if (mode === "newer-first") {
        await inspect(cdp, `window.__docparseExportTiming.pending[${offset + 1}].callback(window.__docparseExportTiming.pending[${offset + 1}].blob)`);
        await page.locator("#export-preview").waitFor({ state: "visible" });
        await inspect(cdp, `window.__docparseExportTiming.pending[${offset}].callback(window.__docparseExportTiming.pending[${offset}].blob)`);
      } else {
        await inspect(cdp, `window.__docparseExportTiming.pending[${offset}].callback(${mode === "older-failure" ? "null" : `window.__docparseExportTiming.pending[${offset}].blob`})`);
        assert.equal(await page.locator("#export-preview").isVisible(), false, "Stale success revealed an old image");
        assert.equal(await page.locator("#export-status").textContent(), "Preparing your image…", "Stale failure changed the new request's status");
        assert.equal(await page.locator("#download").isEnabled(), false, "Stale cleanup unlocked an active export");
        await inspect(cdp, `window.__docparseExportTiming.pending[${offset + 1}].callback(window.__docparseExportTiming.pending[${offset + 1}].blob)`);
      }
      await page.locator("#export-preview").waitFor({ state: "visible" });
      assert.equal(await page.locator("#page-position").textContent(), "Page 2 / 3");
      assert((await page.locator("#export-detail").textContent()).startsWith("Page 2 ·"), "Stale result replaced the current preview");
      assert((await page.locator("#save-export").getAttribute("download")).endsWith("page-2-overlay.png"), "Stale result replaced the filename");
      assert.equal(await inspect(cdp, "window.__docparseExportTiming.urls.size"), 1, "An obsolete export retained a blob URL");
      await page.locator("#close-export").click();
      assert.equal(await inspect(cdp, "window.__docparseExportTiming.urls.size"), 0, "Closing the preview retained its blob URL");
      yield { mode, status: "passed" };
    }
  } finally {
    // Always restore the isolated page, including when an assertion fails.
    try { if (await page.locator("#export-dialog").isVisible()) await page.locator("#close-export").click(); }
    finally {
      await inspect(cdp, `(() => {
        const state = window.__docparseExportTiming;
        HTMLCanvasElement.prototype.toBlob = state.original;
        URL.createObjectURL = state.create; URL.revokeObjectURL = state.revoke;
        for (const url of state.urls) state.revoke.call(URL, url);
        delete window.__docparseExportTiming;
        return true;
      })()`);
    }
  }
}
