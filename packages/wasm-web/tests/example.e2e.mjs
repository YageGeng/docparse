import assert from "node:assert/strict";

/** Checks actual viewport geometry so hiding root overflow cannot conceal clipped results. */
export async function inspectViewport(page) {
  const geometry = await page.evaluate(() => {
    const root = document.documentElement;
    const viewer = document.querySelector("#viewer");
    const workspace = document.querySelector(".workspace").getBoundingClientRect();
    const sheet = document.querySelector(".page-sheet")?.getBoundingClientRect();
    const bounds = viewer.getBoundingClientRect();
    const toolbar = document.querySelector(".viewer-toolbar");
    return {
      width: root.clientWidth, height: root.clientHeight,
      rootWidth: root.scrollWidth, rootHeight: root.scrollHeight,
      workspaceTop: workspace.top, workspaceBottom: workspace.bottom,
      viewerWidth: viewer.clientWidth, viewerHeight: viewer.clientHeight,
      scrollWidth: viewer.scrollWidth, scrollHeight: viewer.scrollHeight,
      toolbarOverflow: toolbar.scrollWidth - toolbar.clientWidth,
      sheet: sheet ? { left: sheet.left - bounds.left, top: sheet.top - bounds.top, right: sheet.right - bounds.left, bottom: sheet.bottom - bounds.top, width: sheet.width, height: sheet.height } : null,
    };
  });
  assert(geometry.rootHeight <= geometry.height + 1 && geometry.rootWidth <= geometry.width + 1, "The app must not scroll outside the viewport");
  assert(geometry.workspaceTop >= 0 && geometry.workspaceBottom <= geometry.height + 1, "The complete workspace must remain visible");
  assert(geometry.viewerHeight > 0 && geometry.toolbarOverflow <= 1, "The page viewer and its controls must remain usable");
  if (geometry.sheet) {
    assert(geometry.sheet.width > 0 && geometry.sheet.height > 0, "Fit mode must render a visible PDF page");
    assert(geometry.scrollHeight <= geometry.viewerHeight + 1 && geometry.scrollWidth <= geometry.viewerWidth + 1, "Fit mode must show the entire PDF without scrolling");
    assert(geometry.sheet.left >= -1 && geometry.sheet.top >= -1 && geometry.sheet.right <= geometry.viewerWidth + 1 && geometry.sheet.bottom <= geometry.viewerHeight + 1, "Fit mode must keep all four page edges inside the viewer");
  }
  return geometry;
}

/** Selects real local bytes through the same file chooser used by a person. */
async function choose(page, path) {
  const pending = page.waitForEvent("filechooser", { timeoutMs: 10000 });
  await page.locator("#choose").click();
  await (await pending).setFiles([path]);
  assert.equal(await page.locator("#file-name").textContent(), path.split("/").at(-1));
}

/** Yields observed UI progress so a caller can report it without blocking for a whole parse. */
async function* ready(page, timeoutMs = 180000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const stage = await page.locator("#stage").textContent();
    const detail = await page.locator("#status-detail").textContent();
    if (stage === "Your document is ready") return;
    assert(!/Could not|errors/.test(stage), `${stage}: ${detail}`);
    yield { kind: "progress", stage, detail };
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  throw new Error("The real PDF parse did not complete before the UI test deadline");
}

/** Exercises the actual example UI with its production SDK, Worker, PDFium and model.
 * The supplied page implements the common Playwright locator/file-chooser surface.
 * No request interception, synthetic results, or replacement parsing backend is used.
 */
export async function* runExampleE2E(page, { pdf, invalidPdf, cancellationPdf, expectedProvider }) {
  assert.equal(await page.locator("#parse").isEnabled(), false);
  assert.equal(await page.locator(".overlay").count(), 0);
  assert.equal(await page.locator("#execution-provider").evaluate(element => element.value), "webgpu");
  await inspectViewport(page);
  yield { kind: "check", name: "Empty state has no stale results and disables parsing" };

  await choose(page, pdf);
  assert(await page.locator("#parse").isEnabled());
  await page.locator("#parse").click();
  assert(await page.locator("#cancel").isVisible());
  assert.equal(await page.locator("#execution-provider").isEnabled(), false);
  yield { kind: "check", name: "File chooser starts a real cancellable parse" };
  yield* ready(page);
  const actualProvider = await page.locator("#engine-status").getAttribute("data-provider");
  if (expectedProvider) assert.equal(actualProvider, expectedProvider);
  assert.equal(await page.locator("#engine-status").textContent(), actualProvider === "webgpu" ? "WebGPU active" : "CPU fallback · WebGPU unavailable");
  yield { kind: "check", name: "GPU-first initialization reports the actual backend", actualProvider };
  assert.equal(await page.locator("#page-count").textContent(), "3");
  assert.equal(await page.locator(".page-thumb img").count(), 3);
  assert.equal(await page.locator(".page-button:not(:disabled)").count(), 3);
  assert((await page.locator(".overlay").count()) > 0);
  yield { kind: "check", name: "Three actual PDFium page images and layout overlays are delivered" };
  yield { kind: "check", name: "The app and entire PDF page fit the viewport", geometry: await inspectViewport(page) };

  await page.locator('.overlay[aria-label="Region 2: paragraph title"]').click();
  assert.equal(await page.locator("#extracted-text").textContent(), "Fixture Page 1");
  assert(await page.locator("#copy").isEnabled());
  assert.equal(await page.locator(".overlay.selected").count(), 1);
  const selectedOverlay = page.locator(".overlay.selected");
  // Exercise actual keyboard focus, which adds an AABB outline to SVG groups by default.
  await selectedOverlay.press("Tab");
  await page.locator(":focus").press("Shift+Tab");
  const focusStyle = await selectedOverlay.evaluate(element => ({ focused: element === document.activeElement, visible: element.matches(":focus-visible"), outline: getComputedStyle(element).outlineStyle, stroke: getComputedStyle(element.querySelector("rect, polygon")).stroke, strokeWidth: getComputedStyle(element.querySelector("rect, polygon")).strokeWidth }));
  assert(focusStyle.focused && focusStyle.visible, "The region must remain keyboard focusable");
  assert.equal(focusStyle.outline, "none", "SVG focus must not draw the enclosing AABB");
  // SVG computed lengths can be expressed in local user units under viewport scaling.
  assert(Number.parseFloat(focusStyle.strokeWidth) > 0);
  assert.equal(focusStyle.stroke, "rgb(23, 103, 183)", "Keyboard focus must remain visible on the contour");
  await selectedOverlay.press("Enter");
  const textVisible = await page.evaluate(() => {
    const bounds = document.querySelector("#extracted-text").getBoundingClientRect();
    return bounds.bottom > 0 && bounds.top < document.documentElement.clientHeight;
  });
  assert(textVisible, "The selected text must be visible, including in the narrow-screen inspector");
  yield { kind: "check", name: "Clicking an actual overlay reveals the expected source text" };

  const originalWidth = await page.evaluate(() => document.querySelector(".page-sheet").getBoundingClientRect().width);
  await page.locator("#zoom-in").click();
  assert.equal(await page.locator("#zoom-fit").textContent(), "125%");
  const zoomedWidth = await page.evaluate(() => document.querySelector(".page-sheet").getBoundingClientRect().width);
  assert(zoomedWidth > originalWidth * 1.2);
  assert.equal(await page.locator("#extracted-text").textContent(), "Fixture Page 1");
  await page.locator("#zoom-fit").click();
  assert.equal(await page.locator("#zoom-fit").textContent(), "100%");
  await inspectViewport(page);
  await page.locator("#toggle-overlays").click();
  assert.equal(await page.locator(".overlay").first().isVisible(), false);
  await page.locator("#toggle-overlays").click();
  assert(await page.locator(".overlay").first().isVisible());
  yield { kind: "check", name: "Zoom and overlay visibility preserve the selected text" };

  await page.locator("#block-select").selectOption({ label: "3. text" });
  assert.equal(await page.locator("#extracted-text").textContent(), "Left column line 1");
  await page.locator("#next").click();
  assert.equal(await page.locator("#page-position").textContent(), "Page 2 / 3");
  assert.equal(await page.locator("#selected-number").textContent(), "—");
  await inspectViewport(page);
  yield { kind: "check", name: "Region menu and page navigation reset selection correctly" };

  await page.locator("#download").click();
  await page.locator("#export-preview").waitFor({ state: "visible", timeoutMs: 10000 });
  const image = await page.evaluate(() => { const node = document.querySelector("#export-preview"); return { complete: node.complete, width: node.naturalWidth, height: node.naturalHeight }; });
  assert(image.complete && image.width > 500 && image.height > 500, "Export preview must decode a real raster");
  assert((await page.locator("#save-export").getAttribute("download")).endsWith("page-2-overlay.png"));
  assert((await page.locator("#save-export").getAttribute("href")).startsWith("blob:"));
  yield { kind: "check", name: "Overlay PNG is generated and decoded in the export preview", image };
  await page.locator("#close-export").click();
  assert.equal(await page.locator("#export-dialog").isVisible(), false);

  await choose(page, cancellationPdf);
  await page.locator("#parse").click();
  await page.locator("#cancel").click();
  assert.equal(await page.locator("#stage").textContent(), "Parsing canceled");
  await new Promise(resolve => setTimeout(resolve, 700));
  assert.equal(await page.locator("#stage").textContent(), "Parsing canceled");
  assert.equal(await page.locator(".overlay").count(), 0);
  yield { kind: "check", name: "Active cancellation ignores late events and leaves a retry action" };

  await choose(page, pdf);
  await page.locator("#parse").click();
  yield* ready(page);
  assert.equal(await page.locator("#page-count").textContent(), "3");
  yield { kind: "check", name: "A new Worker parses successfully after cancellation" };

  await choose(page, invalidPdf);
  await page.locator("#parse").click();
  await page.getByText("Could not parse this PDF", { exact: true }).waitFor({ state: "visible", timeoutMs: 15000 });
  assert.equal(await page.locator(".overlay").count(), 0);
  assert.equal(await page.locator(".page-thumb img").count(), 0);
  assert(await page.locator("#parse").isEnabled());
  yield { kind: "check", name: "Invalid PDF displays a real error without retaining old images or text" };

  await choose(page, pdf);
  await page.locator("#parse").click();
  yield* ready(page);
  assert.equal(await page.locator(".page-thumb img").count(), 3);
  yield { kind: "check", name: "The same example recovers from a bad PDF with a valid document" };

  await page.locator("#execution-provider").selectOption("wasm");
  assert.equal(await page.locator("#engine-status").getAttribute("data-provider"), null);
  assert.equal(await page.locator(".overlay").count(), 0);
  await page.locator("#parse").click();
  yield* ready(page);
  assert.equal(await page.locator("#engine-status").getAttribute("data-provider"), "wasm");
  assert.equal(await page.locator("#engine-status").textContent(), "CPU active");
  assert.equal(await page.locator(".page-thumb img").count(), 3);
  yield { kind: "check", name: "Changing backend recreates the parser and completes a real CPU parse" };
}

/** Visits every rendered page and checks the repeated overlay through visible UI controls.
 * Text expectations belong to the caller's PDF fixture, never to the production parser.
 */
export async function* inspectDocumentPages(page, { pageCount, repeatedText, repeatedTextPages }) {
  assert.equal(await page.locator("#page-count").textContent(), String(pageCount));
  assert.equal(await page.locator(".page-button:not(:disabled)").count(), pageCount);
  for (let number = 1; number <= pageCount; number++) {
    await page.getByRole("button", { name: `Show page ${number}`, exact: true }).click();
    assert.equal(await page.locator("#page-position").textContent(), `Page ${number} / ${pageCount}`);
    let geometry;
    for (let attempt = 0; attempt < 20; attempt++) {
      geometry = await page.evaluate(() => {
        const image = document.querySelector('.page-button[aria-current="page"] img');
        const sheet = document.querySelector(".page-sheet");
        return { width: image.naturalWidth, height: image.naturalHeight, viewBox: sheet.getAttribute("viewBox").split(" ").map(Number) };
      });
      if (geometry.width && geometry.height) break;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    assert(geometry.width > 0 && geometry.height > 0, `Page ${number}: raster did not decode`);
    assert(Math.abs(geometry.width / geometry.height - geometry.viewBox[2] / geometry.viewBox[3]) < .005, `Page ${number}: raster and overlay aspect ratios differ`);
    const regions = await page.locator(".overlay").count();
    assert.equal(await page.locator("#block-select option").count(), regions + 1);
    let text;
    if (number <= repeatedTextPages) {
      // Watermark identity is semantic, independent of box size or model ordering.
      const watermark = page.locator('.overlay[aria-label$=": watermark"]');
      assert.equal(await watermark.count(), 1, `Page ${number}: expected one detached watermark`);
      assert.equal(await watermark.locator("rect").count(), 0, "Watermark must not use the AABB for hit testing");
      const contour = await watermark.locator("polygon").getAttribute("points");
      assert(contour, `Page ${number}: missing exact watermark contour`);
      const points = contour.trim().split(/\s+/).map(pair => pair.split(",").map(Number));
      const xs = points.map(point => point[0]), ys = points.map(point => point[1]);
      const area = Math.abs(points.reduce((sum, point, index) => { const next = points[(index + 1) % points.length]; return sum + point[0] * next[1] - point[1] * next[0]; }, 0)) / 2;
      const boxArea = (Math.max(...xs) - Math.min(...xs)) * (Math.max(...ys) - Math.min(...ys));
      assert(area > 0 && area < boxArea * .3, `Page ${number}: contour still behaves like a broad AABB`);
      const id = await watermark.getAttribute("data-block-id");
      await page.locator("#block-select").selectOption(id);
      if (number === 1) {
        await watermark.press("Tab");
        await page.locator(":focus").press("Shift+Tab");
        const focused = await watermark.evaluate(element => ({ active: element === document.activeElement, outline: getComputedStyle(element).outlineStyle, stroke: getComputedStyle(element.querySelector("polygon")).stroke, strokeWidth: getComputedStyle(element.querySelector("polygon")).strokeWidth }));
        assert(focused.active, "Watermark must retain keyboard focus");
        assert.equal(focused.outline, "none", "Watermark focus must follow its polygon rather than its AABB");
        assert(Number.parseFloat(focused.strokeWidth) > 0);
        assert.equal(focused.stroke, "rgb(23, 103, 183)");
        await watermark.press("Space");
        assert.equal(await watermark.getAttribute("aria-pressed"), "true");
      }
      text = await page.locator("#extracted-text").textContent();
      assert.equal(text.replace(/\s/g, ""), repeatedText.replace(/\s/g, ""), `Page ${number}: repeated overlay text is not independently reachable`);
    }
    else assert.equal(await page.locator('.overlay[aria-label$=": watermark"]').count(), 0, `Page ${number}: unexpected watermark`);
    yield { page: number, regions, geometry, repeatedText: text, status: "passed" };
  }
}
