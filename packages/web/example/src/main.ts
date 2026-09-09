import { createParser, DocParseError } from "../../dist/index.js";
import type { Block, DocParser, DocumentResult, PageImageResult, ParserProgress, ParserTiming } from "../../dist/index.js";

/** The example owns these fixed elements; PDF text is always inserted with textContent. */
const ui = {
  file: document.querySelector<HTMLInputElement>("#file")!,
  name: document.querySelector<HTMLElement>("#file-name")!,
  detail: document.querySelector<HTMLElement>("#file-detail")!,
  parse: document.querySelector<HTMLButtonElement>("#parse")!,
  cancel: document.querySelector<HTMLButtonElement>("#cancel")!,
  provider: document.querySelector<HTMLSelectElement>("#execution-provider")!,
  timingDetails: document.querySelector<HTMLButtonElement>("#timing-details")!,
  timingDialog: document.querySelector<HTMLDialogElement>("#timing-dialog")!,
  timingRows: document.querySelector<HTMLElement>("#timing-rows")!,
  engine: document.querySelector<HTMLElement>("#engine-status")!,
  stage: document.querySelector<HTMLElement>("#stage")!,
  status: document.querySelector<HTMLElement>(".status-card")!,
  symbol: document.querySelector<HTMLElement>("#status-symbol")!,
  statusDetail: document.querySelector<HTMLElement>("#status-detail")!,
  progress: document.querySelector<HTMLProgressElement>("#progress")!,
  progressDetail: document.querySelector<HTMLElement>("#progress-detail")!,
  pages: document.querySelector<HTMLElement>("#pages")!,
  count: document.querySelector<HTMLElement>("#page-count")!,
  position: document.querySelector<HTMLElement>("#page-position")!,
  previous: document.querySelector<HTMLButtonElement>("#previous")!,
  next: document.querySelector<HTMLButtonElement>("#next")!,
  viewer: document.querySelector<HTMLElement>("#viewer")!,
  download: document.querySelector<HTMLButtonElement>("#download")!,
  regions: document.querySelector<HTMLElement>("#region-count")!,
  select: document.querySelector<HTMLSelectElement>("#block-select")!,
  selected: document.querySelector<HTMLElement>("#selected-number")!,
  meta: document.querySelector<HTMLElement>("#selection-meta")!,
  text: document.querySelector<HTMLElement>("#extracted-text")!,
  copy: document.querySelector<HTMLButtonElement>("#copy")!,
  copyStatus: document.querySelector<HTMLElement>("#copy-status")!,
  warnings: document.querySelector<HTMLElement>("#page-warnings")!,
  characters: document.querySelector<HTMLElement>("#character-count")!,
  zoomIn: document.querySelector<HTMLButtonElement>("#zoom-in")!,
  zoomOut: document.querySelector<HTMLButtonElement>("#zoom-out")!,
  zoomFit: document.querySelector<HTMLButtonElement>("#zoom-fit")!,
  overlays: document.querySelector<HTMLButtonElement>("#toggle-overlays")!,
  exportDialog: document.querySelector<HTMLDialogElement>("#export-dialog")!,
  exportImage: document.querySelector<HTMLImageElement>("#export-preview")!,
  exportStatus: document.querySelector<HTMLElement>("#export-status")!,
  exportDetail: document.querySelector<HTMLElement>("#export-detail")!,
  saveExport: document.querySelector<HTMLAnchorElement>("#save-export")!,
};
const initialView = ui.viewer.cloneNode(true) as HTMLElement;
const initialPages = ui.pages.cloneNode(true) as HTMLElement;
const previews = new Map<number, PageImageResult & { url: string }>();
const pageButtons = new Map<number, HTMLButtonElement>();
const timingTotals = new Map<string, { count: number; total: number; first: number }>();
let parser: DocParser | undefined;
let selectedFile: File | undefined;
let result: DocumentResult | undefined;
let controller: AbortController | undefined;
let generation = 0, pageNumber = 1, pageCount = 0;
let selectedBlock: Block | undefined;
let busy = false;
let zoom = 1;
/** Identity and resources of one export, independent of the PDF parse generation. */
type ImageExport = { url?: string };
let activeExport: ImageExport | undefined;

/** Updates one real operation stage; absent fractions intentionally remain indeterminate. */
function status(title: string, detail: string, state = "idle", fraction?: number): void {
  ui.stage.textContent = title;
  ui.statusDetail.textContent = detail;
  ui.status.dataset.state = state;
  ui.symbol.textContent = state === "error" ? "!" : state === "done" ? "✓" : state === "busy" ? "◌" : "○";
  ui.progress.hidden = state !== "busy";
  if (fraction === undefined) ui.progress.removeAttribute("value");
  else ui.progress.value = Math.min(100, Math.max(0, fraction * 100));
  ui.progressDetail.textContent = fraction === undefined ? "" : `${Math.round(fraction * 100)}%`;
}

/** Enables navigation only for rasters already delivered by the current parse. */
function controls(): void {
  ui.parse.disabled = !selectedFile || busy;
  ui.provider.disabled = busy;
  ui.cancel.hidden = !busy;
  ui.parse.hidden = busy;
  ui.previous.disabled = !previews.has(pageNumber - 1);
  ui.next.disabled = !previews.has(pageNumber + 1);
  ui.download.disabled = busy || Boolean(activeExport) || !result || !previews.has(pageNumber);
  ui.zoomIn.disabled = !previews.has(pageNumber) || zoom >= 2;
  ui.zoomOut.disabled = !previews.has(pageNumber) || zoom <= .75;
  ui.zoomFit.disabled = !previews.has(pageNumber);
  ui.overlays.disabled = !result || !previews.has(pageNumber);
}

/** Builds a bounded set of page buttons once the actual page count becomes available. */
function setPageCount(count: number): void {
  if (pageCount === count) return;
  pageCount = count;
  ui.count.textContent = String(count);
  ui.pages.replaceChildren();
  pageButtons.clear();
  for (let number = 1; number <= count; number++) {
    const button = document.createElement("button");
    button.className = "page-button"; button.disabled = true;
    const thumbnail = document.createElement("span"); thumbnail.className = "page-thumb";
    const caption = document.createElement("span"); caption.className = "page-caption"; caption.textContent = `Page ${number}`;
    button.append(thumbnail, caption);
    button.setAttribute("aria-label", `Show page ${number}`);
    const detail = document.createElement("small"); detail.textContent = "Waiting"; button.append(detail);
    button.addEventListener("click", () => showPage(number));
    ui.pages.append(button); pageButtons.set(number, button);
  }
}

/** Clears document-owned blobs while keeping a ready model available for another PDF. */
function clearDocument(): void {
  timingTotals.clear(); ui.timingDetails.disabled = true; ui.timingDialog.close();
  for (const preview of previews.values()) URL.revokeObjectURL(preview.url);
  previews.clear(); result = undefined; pageNumber = 1; selectedBlock = undefined;
  setPageCount(0); ui.pages.replaceChildren(...Array.from(initialPages.cloneNode(true).childNodes));
  ui.viewer.replaceChildren(...Array.from(initialView.cloneNode(true).childNodes));
  ui.position.textContent = "Page —";
  ui.regions.textContent = "0 regions";
  ui.select.replaceChildren(new Option("Select a region…", "")); ui.select.disabled = true;
  ui.selected.textContent = "—"; ui.meta.textContent = "A closer look, one region at a time.";
  ui.text.textContent = "Select an overlay to reveal the original text from your PDF.";
  ui.copy.disabled = true; ui.copyStatus.textContent = ""; ui.warnings.hidden = true;
  ui.characters.textContent = "—"; document.body.dataset.hasSelection = "false";
  zoom = 1; ui.zoomFit.textContent = "100%";
  ui.viewer.classList.remove("overlays-hidden"); ui.overlays.setAttribute("aria-pressed", "true");
  closeExport();
}

/** Cancels the active Worker and invalidates every pending callback before another run starts. */
function cancel(): void {
  generation++;
  controller?.abort(); controller = undefined;
  if (busy) { void parser?.close(); parser = undefined; }
  if (!parser) { ui.engine.textContent = "Engine stopped"; delete ui.engine.dataset.provider; }
  busy = false;
  status("Parsing canceled", "Select Parse document to start again.");
  controls();
}

/** Selects local bytes without uploading the PDF or leaving stale results visible. */
function choose(file: File | undefined): void {
  if (!file) return;
  if (busy) cancel();
  else generation++;
  clearDocument();
  selectedFile = file;
  document.body.dataset.hasFile = "true";
  ui.name.textContent = file.name; ui.name.title = file.name;
  ui.detail.textContent = `${file.size < 1024 * 1024 ? `${(file.size / 1024).toFixed(1)} KB` : `${(file.size / 1024 / 1024).toFixed(2)} MB`} · Local PDF`;
  status("Document selected", "Start parsing to inspect the page structure.");
  controls();
}

/** Displays measured download bytes and actual per-stage page counts. */
function progress(event: ParserProgress): void {
  switch (event.stage) {
    case "loading_runtime": status("Loading the parser", "Preparing PDFium and WebAssembly…", "busy"); break;
    case "downloading":
      if (event.artifact === "model") status("Downloading the layout model", `${(event.loaded / 1024 / 1024).toFixed(1)}${event.total ? ` / ${(event.total / 1024 / 1024).toFixed(1)}` : ""} MB`, "busy", event.total ? event.loaded / event.total : undefined);
      break;
    case "initializing_model": status("Preparing the layout model", "Creating the inference session. This can take a moment on the first run.", "busy"); break;
    case "opening": status("Opening your PDF", "Reading the document with PDFium…", "busy"); break;
    case "scanning":
      setPageCount(event.total);
      status("Extracting native text", `${event.completed} of ${event.total} pages scanned`, "busy", event.completed / event.total);
      break;
    case "analyzing":
      status("Analyzing page structure", `${event.completed} of ${event.total} pages analyzed`, "busy", event.completed / event.total);
      break;
    case "linking": status("Assembling the document", "Connecting page results and validating reading order…", "busy"); break;
    case "complete": status("Finishing page previews", `${event.total} pages analyzed`, "busy", 1); break;
  }
}

/** Creates SVG nodes without parsing document-controlled markup. */
function svg<K extends keyof SVGElementTagNameMap>(tag: K, attributes: Record<string, string>): SVGElementTagNameMap[K] {
  const node = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
  return node;
}

/** Uses a stable small palette to distinguish text, headings, tables and images. */
function color(label: string): string {
  if (label === "watermark") return "#9c6c91";
  if (["doc_title", "paragraph_title", "header"].includes(label)) return "#8a69b1";
  if (["table", "chart"].includes(label)) return "#c88b32";
  if (["image", "header_image", "footer_image"].includes(label)) return "#5585bd";
  return "#238778";
}

/** Selects one canonical block from either the overlay or the accessible region menu. */
function selectBlock(id: string): void {
  const page = result?.pages.find(page => page.page_number === pageNumber);
  selectedBlock = page?.blocks.find(block => block.id === id);
  document.body.dataset.hasSelection = String(Boolean(selectedBlock));
  ui.select.value = selectedBlock?.id ?? "";
  for (const group of Array.from(ui.viewer.querySelectorAll<SVGGElement>(".overlay"))) {
    const selected = group.dataset.blockId === selectedBlock?.id;
    group.classList.toggle("selected", selected); group.setAttribute("aria-pressed", String(selected));
  }
  ui.selected.textContent = selectedBlock ? String(selectedBlock.final_order + 1) : "—";
  ui.meta.textContent = selectedBlock ? `${selectedBlock.label.replaceAll("_", " ")} · Page ${pageNumber}` : "A closer look, one region at a time.";
  ui.text.textContent = selectedBlock?.label === "reference"
    ? "Visual reference area. Select a reference content region to read its text."
    : selectedBlock ? selectedBlock.text || "No native text was extracted for this region. Image and outline text require OCR." : "Select an overlay to reveal the original text from your PDF.";
  if (selectedBlock?.table) {
    const source = selectedBlock.table;
    const table = document.createElement("table"); table.className = "table-view";
    table.setAttribute("aria-label", `Table with ${source.row_count} rows and ${source.column_count} columns`);
    const body = document.createElement("tbody");
    for (let row = 0; row < source.row_count; row++) {
      const tr = document.createElement("tr");
      for (const cell of source.cells.filter(cell => cell.row === row).sort((a, b) => a.column - b.column)) {
        // Source PDF text is never parsed as markup; spans come from the validated Rust grid.
        const td = document.createElement(cell.is_header ? "th" : "td");
        td.rowSpan = cell.row_span; td.colSpan = cell.column_span; td.textContent = cell.text;
        tr.append(td);
      }
      body.append(tr);
    }
    table.append(body); ui.text.replaceChildren(table);
    ui.meta.textContent += ` · ${source.row_count} rows × ${source.column_count} columns`;
  }
  ui.characters.textContent = selectedBlock ? `${Array.from(selectedBlock.text).length} characters` : "—";
  ui.copy.disabled = !selectedBlock?.text; ui.copyStatus.textContent = "";
}

/** Fits the raster and overlay to the same viewport; pixel dimensions never become PDF coordinates. */
function showPage(number: number): void {
  const preview = previews.get(number);
  if (!preview) return;
  const changedPage = pageNumber !== number;
  pageNumber = number;
  const page = result?.pages.find(page => page.page_number === number);
  // Before analysis finishes, the raster's aspect ratio is sufficient for a preview.
  const width = page?.width ?? preview.width, height = page?.height ?? preview.height;
  const sheet = svg("svg", { viewBox: `0 0 ${width} ${height}`, class: "page-sheet", role: "group", "aria-label": `Page ${number} with selectable regions` });
  // CSS fits both axes to the viewer and responds to viewport changes without rebuilding regions.
  sheet.style.setProperty("--page-ratio", String(width / height));
  sheet.style.setProperty("--zoom", String(zoom));
  sheet.append(svg("image", { href: preview.url, width: String(width), height: String(height) }));
  ui.select.replaceChildren(new Option("Select a region…", ""));
  for (const block of page?.blocks ?? []) ui.select.add(new Option(`${block.final_order + 1}. ${block.label.replaceAll("_", " ")}`, block.id));
  // Large boxes go behind smaller ones: a page-spanning watermark must not swallow
  // clicks on body paragraphs. The region menu makes every overlapping block reachable.
  const blocks = [...(page?.blocks ?? [])].sort((a, b) =>
    (b.bbox.right - b.bbox.left) * (b.bbox.bottom - b.bbox.top) - (a.bbox.right - a.bbox.left) * (a.bbox.bottom - a.bbox.top));
  for (const block of blocks) {
    const bounds = block.bbox;
    const group = svg("g", { class: "overlay", tabindex: "0", role: "button", "aria-label": `Region ${block.final_order + 1}: ${block.label.replaceAll("_", " ")}`, "aria-pressed": "false" });
    group.dataset.blockId = block.id;
    if (block.label === "reference") group.classList.add("reference-overlay");
    // The actual footprint owns hit testing; empty AABB corners must not catch pointer clicks.
    group.append(block.polygon?.length ? svg("polygon", { points: block.polygon.map(point => `${point.x},${point.y}`).join(" "), stroke: color(block.label), fill: color(block.label) }) : svg("rect", { x: String(bounds.left), y: String(bounds.top), width: String(bounds.right - bounds.left), height: String(bounds.bottom - bounds.top), stroke: color(block.label), fill: color(block.label) }));
    const label = svg("text", { x: String(Math.max(1, bounds.left + 2)), y: String(Math.max(7, bounds.top + 7)), fill: color(block.label) });
    label.textContent = String(block.final_order + 1); group.append(label);
    group.addEventListener("click", () => selectBlock(block.id));
    group.addEventListener("keydown", event => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectBlock(block.id); } });
    sheet.append(group);
  }
  ui.viewer.replaceChildren(sheet);
  if (changedPage) { ui.viewer.scrollTop = 0; ui.viewer.scrollLeft = 0; }
  ui.position.textContent = `Page ${number} / ${pageCount}`;
  ui.regions.textContent = `${page?.blocks.length ?? 0} regions`;
  ui.select.disabled = !page?.blocks.length;
  for (const [index, button] of pageButtons) {
    if (index === number) button.setAttribute("aria-current", "page"); else button.removeAttribute("aria-current");
  }
  pageButtons.get(number)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  ui.warnings.hidden = !page?.warnings.length;
  // A model failure is a different result from missing native text; make degraded layout visible.
  ui.warnings.textContent = page?.warnings.some(warning => warning.code === "LayoutUnavailable")
    ? "Layout inference failed on this page. A geometry-only fallback is shown."
    : page?.warnings.some(warning => warning.code === "TableStructureUnavailable")
      ? "Some table structures could not be recovered. Their original text is preserved."
      : page?.warnings.length ? `${page.warnings.length} parser warnings on this page. Some regions may have no native text.` : "";
  selectBlock(""); controls();
}

/** Aggregates lightweight observations; detailed page numbers remain available in the SDK callback. */
function recordTiming(scope: string, event: ParserTiming): void {
  const key = `${scope} · ${event.stage.replaceAll("_", " ")}`;
  const entry = timingTotals.get(key) ?? { count: 0, total: 0, first: event.duration_ms };
  entry.count++; entry.total += event.duration_ms; timingTotals.set(key, entry);
  ui.timingDetails.disabled = false;
}

/** Opens a snapshot without adding height or automatic scrolling to the PDF workspace. */
function showTimings(): void {
  ui.timingRows.replaceChildren();
  for (const [stage, entry] of timingTotals) {
    const row = document.createElement("tr");
    for (const value of [stage, String(entry.count), `${entry.total.toFixed(1)} ms`, `${(entry.total / entry.count).toFixed(1)} ms`, `${entry.first.toFixed(1)} ms`]) {
      const cell = document.createElement("td"); cell.textContent = value; row.append(cell);
    }
    ui.timingRows.append(row);
  }
  ui.timingDialog.showModal();
}

/** Runs one cancellable operation while keeping stale callbacks from replacing a newer document. */
async function parse(): Promise<void> {
  if (!selectedFile || busy) return;
  const file = selectedFile;
  clearDocument(); busy = true; controls();
  const run = ++generation;
  const signal = (controller = new AbortController()).signal;
  const started = performance.now();
  try {
    let current = parser;
    if (!current) {
      ui.engine.textContent = ui.provider.value === "webgpu" ? "Preparing WebGPU…" : "Preparing CPU…";
      delete ui.engine.dataset.provider;
      current = await createParser({
        artifacts: { kind: "urls", model: new URL("../models/inference.onnx", location.href).href, config: new URL("../models/inference.yml", location.href).href, manifest: new URL("../models/model-manifest.json", location.href).href },
        // Request acceleration explicitly; show the actual backend if CPU fallback is needed.
        executionProvider: ui.provider.value === "webgpu" ? "webgpu" : "wasm",
        allowCpuFallback: true,
        config: { render: { dpi: 144, max_long_edge_pixels: 2000 } },
        signal, onProgress: event => { if (run === generation) progress(event); },
        onTiming: event => { if (run === generation) recordTiming("Initialization", event); },
      });
      if (run !== generation) { await current.close(); return; }
      parser = current;
    }
    ui.engine.dataset.provider = current.executionProvider;
    ui.engine.textContent = current.executionProvider === "webgpu" ? "WebGPU active" : ui.provider.value === "webgpu" ? "CPU fallback · WebGPU unavailable" : "CPU active";
    const bytes = new Uint8Array(await file.arrayBuffer());
    if (run !== generation) return;
    const parsed = await current.parse(bytes, {
      signal,
      onProgress: event => { if (run === generation) progress(event); },
      onTiming: event => { if (run === generation) recordTiming("Document", event); },
      onPageImage: image => {
        if (run !== generation) return;
        const previous = previews.get(image.pageNumber);
        if (previous) URL.revokeObjectURL(previous.url);
        previews.set(image.pageNumber, { ...image, url: URL.createObjectURL(image.blob) });
        const button = pageButtons.get(image.pageNumber);
        if (button) {
          button.disabled = false; button.querySelector("small")!.textContent = "Preview ready";
          const thumbnail = document.createElement("img"); thumbnail.src = previews.get(image.pageNumber)!.url;
          thumbnail.alt = ""; thumbnail.loading = "lazy"; thumbnail.decoding = "async";
          button.querySelector(".page-thumb")!.replaceChildren(thumbnail);
        }
        if (image.pageNumber === pageNumber) showPage(pageNumber);
        controls();
      },
    });
    if (run !== generation) return;
    result = parsed;
    for (const page of parsed.pages) {
      const button = pageButtons.get(page.page_number);
      if (button) button.querySelector("small")!.textContent = previews.has(page.page_number) ? `${page.blocks.length} regions` : "Preview unavailable";
    }
    showPage(pageNumber);
    const elapsed = ((performance.now() - started) / 1000).toFixed(1);
    status(parsed.errors.length ? "Finished with page errors" : "Your document is ready", `${parsed.pages.length} pages · ${parsed.pages.reduce((sum, page) => sum + page.blocks.length, 0)} regions · ${elapsed}s${parsed.errors.length ? ` · ${parsed.errors.length} page errors` : ""}`, parsed.errors.length ? "error" : "done");
  } catch (error) {
    if (run !== generation) return;
    status("Could not parse this PDF", error instanceof Error ? error.message : String(error), "error");
    if (!(error instanceof DocParseError) || ["ParserClosed", "WorkerStopped", "Aborted"].includes(error.code)) { void parser?.close(); parser = undefined; }
    if (!parser) { ui.engine.textContent = "Engine unavailable"; delete ui.engine.dataset.provider; }
  } finally {
    if (run === generation) { busy = false; controller = undefined; controls(); }
  }
}

/** Previews the actual overlay PNG before a user-initiated save, including embedded browsers. */
async function download(): Promise<void> {
  const page = result?.pages.find(page => page.page_number === pageNumber);
  const preview = previews.get(pageNumber);
  if (!page || !preview || activeExport) return;
  const run = generation;
  const request: ImageExport = {};
  activeExport = request;
  const filename = `${selectedFile?.name.replace(/\.pdf$/i, "") ?? "document"}-page-${page.page_number}-overlay.png`;
  controls();
  ui.exportStatus.textContent = "Preparing your image…"; ui.exportStatus.hidden = false;
  ui.exportDetail.textContent = `Page ${page.page_number} · Preparing PNG`;
  ui.exportImage.hidden = true; ui.saveExport.hidden = true;
  ui.exportDialog.showModal();
  try {
    const bitmap = await createImageBitmap(preview.blob);
    if (activeExport !== request || run !== generation || !ui.exportDialog.open) { bitmap.close(); return; }
    const canvas = document.createElement("canvas"); canvas.width = bitmap.width; canvas.height = bitmap.height;
    const context = canvas.getContext("2d");
    if (!context) { bitmap.close(); throw new Error("Image export needs a 2D canvas"); }
    try { context.drawImage(bitmap, 0, 0); }
    finally { bitmap.close(); }
    context.scale(canvas.width / page.width, canvas.height / page.height);
    for (const block of page.blocks) {
      const b = block.bbox;
      context.strokeStyle = color(block.label); context.lineWidth = .8;
      context.setLineDash(block.label === "reference" ? [4, 3] : []);
      if (block.polygon?.length) {
        // Export the same contour used by the interactive SVG, without rotating an AABB.
        context.beginPath();
        block.polygon.forEach((point, index) => { if (index === 0) context.moveTo(point.x, point.y); else context.lineTo(point.x, point.y); });
        context.closePath(); context.stroke();
      } else context.strokeRect(b.left, b.top, b.right - b.left, b.bottom - b.top);
      context.font = "6px monospace"; context.fillStyle = color(block.label);
      context.fillText(String(block.final_order + 1), Math.max(1, b.left + 2), Math.max(7, b.top + 7));
    }
    const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob(blob => blob ? resolve(blob) : reject(new Error("PNG encoding failed")), "image/png"));
    // A reopened dialog belongs to a new request even when the PDF is unchanged.
    // Check identity before creating a URL so obsolete encodes cannot leak resources
    // or overwrite a newer preview, filename, error message, or busy state.
    if (activeExport !== request || run !== generation || !ui.exportDialog.open) return;
    request.url = URL.createObjectURL(blob);
    ui.exportImage.src = request.url; ui.exportImage.hidden = false; ui.exportStatus.hidden = true;
    ui.exportDetail.textContent = `Page ${page.page_number} · ${canvas.width} × ${canvas.height} px · ${(blob.size / 1024).toFixed(0)} KB`;
    ui.saveExport.href = request.url; ui.saveExport.download = filename; ui.saveExport.hidden = false;
  } catch (error) {
    if (activeExport === request && run === generation && ui.exportDialog.open) {
      ui.exportStatus.textContent = error instanceof Error ? error.message : String(error);
    }
  } finally { if (activeExport === request) controls(); }
}

/** Invalidates an export synchronously and releases only that request's owned URL. */
function closeExport(): void {
  const request = activeExport;
  activeExport = undefined;
  if (request?.url) URL.revokeObjectURL(request.url);
  ui.exportImage.removeAttribute("src"); ui.exportImage.hidden = true;
  ui.saveExport.removeAttribute("href"); ui.saveExport.removeAttribute("download"); ui.saveExport.hidden = true;
  if (ui.exportDialog.open) ui.exportDialog.close();
  controls();
}

/** Scales image and overlay together while leaving canonical PDF geometry untouched. */
function setZoom(value: number): void {
  zoom = Math.min(2, Math.max(.75, value));
  const sheet = ui.viewer.querySelector<SVGSVGElement>(".page-sheet");
  if (sheet) sheet.style.setProperty("--zoom", String(zoom));
  ui.zoomFit.textContent = `${Math.round(zoom * 100)}%`;
  controls();
}

ui.file.addEventListener("change", () => choose(ui.file.files?.[0]));
document.querySelector("#choose")!.addEventListener("click", () => ui.file.click());
ui.parse.addEventListener("click", () => { void parse(); });
ui.cancel.addEventListener("click", cancel);
/** Releases the old session before a different backend can parse the selected document. */
ui.provider.addEventListener("change", () => {
  if (busy) return;
  generation++; void parser?.close(); parser = undefined;
  clearDocument(); delete ui.engine.dataset.provider;
  ui.engine.textContent = ui.provider.value === "webgpu" ? "GPU preferred · CPU fallback if unavailable" : "CPU selected";
  status("Layout engine selected", "Start parsing to initialize the selected engine.");
  controls();
});
ui.previous.addEventListener("click", () => showPage(pageNumber - 1));
ui.next.addEventListener("click", () => showPage(pageNumber + 1));
ui.select.addEventListener("change", () => selectBlock(ui.select.value));
ui.download.addEventListener("click", () => { void download(); });
ui.zoomIn.addEventListener("click", () => setZoom(zoom + .25));
ui.zoomOut.addEventListener("click", () => setZoom(zoom - .25));
ui.zoomFit.addEventListener("click", () => { setZoom(1); ui.viewer.scrollLeft = 0; ui.viewer.scrollTop = 0; });
ui.overlays.addEventListener("click", () => {
  const hidden = ui.viewer.classList.toggle("overlays-hidden");
  ui.overlays.setAttribute("aria-pressed", String(!hidden));
});
document.querySelector("#close-inspector")!.addEventListener("click", () => selectBlock(""));
document.querySelector("#close-export")!.addEventListener("click", closeExport);
ui.exportDialog.addEventListener("cancel", event => { event.preventDefault(); closeExport(); });
ui.exportDialog.addEventListener("close", () => {
  // Native close events are queued. A delayed event from an old dialog must not
  // clear a newly opened export that has already acquired its own request identity.
  if (!ui.exportDialog.open) closeExport();
});
ui.copy.addEventListener("click", async () => {
  if (!selectedBlock?.text) return;
  try { await navigator.clipboard.writeText(selectedBlock.text); ui.copyStatus.textContent = "Copied to clipboard"; }
  catch { ui.copyStatus.textContent = "Clipboard unavailable. Select the text above to copy it."; }
});
// A cached document keeps its page images when returning through browser history.
// Revoke URLs on replacement or export close; full document disposal releases the rest.
window.addEventListener("pagehide", () => {
  closeExport();
  generation++; controller?.abort(); controller = undefined;
  void parser?.close(); parser = undefined;
  ui.engine.textContent = "Engine stopped"; delete ui.engine.dataset.provider;
  if (busy) { busy = false; status("Parsing canceled", "Select Parse document to start again."); controls(); }
});

ui.timingDetails.addEventListener("click", showTimings);
document.querySelector("#close-timing")!.addEventListener("click", () => ui.timingDialog.close());
